# rowdy

A terminal SQL client built with [ratatui](https://github.com/ratatui/ratatui),
[edtui](https://github.com/preiter93/edtui), and [sqlx](https://github.com/launchbadge/sqlx).
The goal is a fast, modal, keyboard-first workspace for writing queries,
exploring schemas, and inspecting results — all without leaving the terminal.

> **Status:** early. The async event loop, query worker, and SQLite,
> Postgres, and MySQL/MariaDB drivers are wired end-to-end. The export
> feature is still ahead.

## What's working today

- **Three-pane layout**
  - **Editor** (left): a vim-mode SQL editor powered by edtui.
  - **Schema browser** (right): a collapsible tree of catalogs / schemas /
    tables / views / columns / indices, populated **lazily** from the live
    connection — each level loads on first expand.
  - **Status / command bar** (bottom): vim-style modeline that doubles as a
    `:command` prompt and as the run-confirmation prompt.
- **Async query execution** through a tokio worker. The UI never blocks on
  the database; a single in-flight query is enforced and `:cancel` aborts it.
- **Confirm-before-run**: `<Space>r` highlights the SQL statement under the
  cursor and asks before executing it. `<Space>R` bypasses the confirmation;
  in editor Visual mode `<Space>r` runs the explicit selection straight away.
- **Typed result cells** (`Null / Bool / Int / Float / Text / Bytes /
  Timestamp / Date / Time / Uuid / Other`) — preserved end-to-end so future
  exporters (CSV / TSV / JSON / SQL inserts) keep type fidelity. The TUI
  renders each cell via its own `display()`; `NULL` cells are dimmed.
- **Three SQL drivers** sharing the same `Datasource` trait:
  - **SQLite** — in-memory or file-based, schema via `sqlite_master` and
    `pragma_*` virtual tables.
  - **Postgres** — schema via `pg_namespace` + `information_schema`, indices
    via `pg_class`/`pg_index` for the uniqueness flag.
  - **MySQL / MariaDB** — schema via `information_schema`, `column_type` for
    declared types (preserves `unsigned`, display widths, etc.).
- **Two themes** (Dark / Light) switchable at runtime, both tuned for high
  text contrast. Theme + schema-panel width persist to
  `./.rowdy/config.toml` (lazily — the file is only created the first time
  you change a default).
- **File logger** at `./.rowdy/<datetime>.log`. The app and every datasource
  write into the same file (connect / execute / cancel / errors).
- **Vim-style modal input** end-to-end: editor uses real vim bindings via
  edtui; the schema panel and result viewer use the same `hjkl` / `gg` / `G`
  vocabulary.

## Architecture

The codebase is a small, MVC-flavoured loop with an async worker on the side:

```
            ┌──────────────────────────────────────────────────┐
            │  tokio runtime                                   │
            │                                                  │
            │  main task (event loop)                          │
            │  ┌───────────────────────────────────────────┐   │
            │  │ select!:                                  │   │
            │  │   crossterm EventStream  → Action         │   │
            │  │   worker → app channel   → Action::Worker │   │
            │  └───────────────────────────────────────────┘   │
            │              │                  ▲                │
            │      cmd_tx  │                  │  evt_rx        │
            │              ▼                  │                │
            │  worker task                                     │
            │  ┌───────────────────────────────────────────┐   │
            │  │ owns Arc<dyn Datasource> (sqlx::Pool)     │   │
            │  │ tracks current query JoinHandle           │   │
            │  │ dispatches Execute / Cancel / Introspect  │   │
            │  └───────────────────────────────────────────┘   │
            └──────────────────────────────────────────────────┘
```

- `App` (`src/app.rs`) owns the entire UI state and the `cmd_tx` handle.
- `Action` (`src/action.rs`) enumerates every legal mutation; `apply()` is
  the single dispatcher.
- `event::translate` (`src/event.rs`) is a pure function that turns a
  `crossterm::Event` into an `Action` based on the current `Mode` and
  `Focus`.
- View functions under `src/ui/` derive entirely from `App` — they never
  mutate state.
- `Datasource` (`src/datasource/mod.rs`) is the cross-driver trait:
  `introspect_catalogs`, `introspect_schemas`, `introspect_tables`,
  `introspect_columns`, `introspect_indices`, `execute`, `cancel`, `close`.
  Drivers live under `src/datasource/sql/`.
- The worker (`src/worker/mod.rs`) owns the live connection pool, runs at
  most one query at a time (with cancellation via `JoinHandle::abort`), and
  fans introspection out concurrently.

State is encoded so that invalid combinations are unrepresentable wherever
possible:

- `Focus { Editor, Schema }` — exactly one panel owns input.
- `Mode { Normal, Command(CommandBuffer), ResultExpanded { id, cursor,
  col_offset }, ConfirmRun { statement } }` — no "expanded but no result"
  state, no "in command mode but no buffer", no "awaiting confirmation but
  no statement".
- `QueryStatus { Idle, Running, Succeeded, Failed, Cancelled }` — replaces a
  bag of booleans / `Option<String>` fields.
- `ResultPayload { Clipped, Full }` — variant says whether more rows exist;
  no `is_clipped` flag.
- `LoadState { NotLoaded, Loading, Loaded, Failed(error) }` on every schema
  node — drives the lazy-load UX without any "is_loading + error" pairs.
- `IntrospectTarget` — a single value identifies both *which level* to load
  and *which DB entity* it belongs to, so worker events reattach to the
  right node deterministically.

## Connection strings

`--connection <url>` is **required**. URL scheme dispatches to the driver:

| Scheme                       | Driver     | Example                                                |
|------------------------------|------------|--------------------------------------------------------|
| `sqlite:`                    | SQLite     | `sqlite:./sample.db`, `sqlite::memory:?cache=shared`   |
| `postgres:` / `postgresql:`  | Postgres   | `postgres://user:pass@host:5432/db`                    |
| `mysql:` / `mariadb:`        | MySQL      | `mysql://user:pass@host:3306/db`                       |

`mariadb://` is rewritten to `mysql://` before sqlx sees it — same wire
protocol, same driver. `postgres://` and `postgresql://` are interchangeable.

> **In-memory SQLite caveat:** the worker uses a connection pool, and each
> SQLite memory connection gets its *own* database unless you opt into
> shared cache. Use `sqlite::memory:?cache=shared` (or a file path) so
> introspection sees the data your queries created.

> **System schemas are hidden** by default — Postgres `pg_catalog`,
> `information_schema`, `pg_toast`, `pg_temp_*`; MySQL `information_schema`,
> `mysql`, `performance_schema`, `sys`. You can still query them by name.

> **Cancel** for Postgres and MySQL currently relies on aborting the
> client-side `JoinHandle` — the future is dropped but the server may keep
> running the query. Real cancellation (`pg_cancel_backend` / `KILL QUERY`)
> needs the worker to track the backend PID / connection id and is on the
> roadmap.

## Running it

```sh
cargo run -- --connection sqlite:./sample.db
```

Requires Rust 2024 edition (≥ 1.85) and a terminal that supports truecolor
for accurate theme rendering.

### Non-TUI connection management

You can manage saved connections without launching the TUI:

```sh
rowdy connections list
rowdy connections add <name> --url <url> [--password <pw>]
rowdy connections edit <name> --url <url> [--password <pw>]   # overwrite
rowdy connections delete <name>
```

Password handling mirrors the TUI:

- **Flag absent** — prompts on stdin (masked). `rpassword` falls back to
  reading from a pipe if stdin isn't a TTY.
- **`--password X`** (non-empty) — uses `X`. On a fresh store this also
  initialises the crypto block.
- **`--password ""`** — explicit "no encryption". Only valid against an empty
  store or an existing plaintext store; refused against an encrypted one.

`list` and `delete` never touch the password — they're pure config edits.

### Per-directory state: `.rowdy/`

On startup rowdy creates `.rowdy/` in the current working directory if it
doesn't exist. It holds:

- `config.toml` — theme + schema panel width. Written **lazily** on the
  first change away from defaults; a vanilla session never creates it.
- `<datetime>.log` — one file per session, named for the launch time.
  Append-only. The app and every datasource log into it (connect /
  execute / cancel / errors). URL passwords are redacted.

### Sample database

A seed program creates a small e-commerce schema (4 tables, 1 view, 5
indices, ~450 rows) so you have something realistic to poke at:

```sh
cargo run --example seed_sqlite -- ./sample.db
cargo run -- --connection sqlite:./sample.db
```

Re-running the seeder is safe — it drops and re-creates the tables.

## Keybindings

rowdy uses **vim-style bindings everywhere**. Three layers determine what a
key does: the global app `Mode`, the focused panel, and the editor's own vim
mode (Normal / Insert / Visual / Search).

### Global

Available wherever the editor is in vim Normal or Visual mode (or the schema
panel is focused).

| Keys                  | Action                                                |
|-----------------------|-------------------------------------------------------|
| `:`                   | Open command prompt                                   |
| `Ctrl+W` then `h`/`l` | Focus editor / schema                                 |
| `Ctrl+W` then `<`/`>` | Grow / shrink schema panel width                      |
| `Ctrl+C`              | Panic exit (use `:q` for a clean quit)                |

### Clipboard

In any input modal (auth prompt, connection form, `:` command prompt) the
standard system-clipboard shortcuts are wired up:

| Keys                                                    | Action |
|---------------------------------------------------------|--------|
| `Ctrl+V` / `Ctrl+Shift+V` / `Cmd+V`                     | Paste  |
| `Ctrl+C` / `Ctrl+Shift+C` / `Cmd+C` (with selection)    | Copy   |
| `Ctrl+X` / `Ctrl+Shift+X` / `Cmd+X` (with selection)    | Cut    |

Copy is suppressed in the password prompt — exposing the masked buffer
would defeat the masking. Bracketed paste from the terminal (which is what
most macOS terminals deliver for `Cmd+V`) is also accepted.

In the SQL editor, edtui's vim bindings drive the clipboard: `y` yanks,
`p` pastes, `d` cuts. They go through the system clipboard automatically
(via `arboard`), so you can yank in rowdy and paste into another app, or
vice versa.

### Editor — leader chord (Space)

Triggered when the editor is in vim Normal or Visual mode.

| Keys        | Action                                                          |
|-------------|-----------------------------------------------------------------|
| `<Space> r` | (Normal) Highlight the statement under the cursor, prompt to run |
| `<Space> r` | (Visual) Run the current selection — no prompt                  |
| `<Space> R` | Run the statement under the cursor immediately — no prompt      |
| `<Space> e` | Expand the latest result to full view                           |
| `<Space> c` | Cancel the in-flight query                                      |
| `<Space> t` | Toggle Dark / Light theme                                       |

The editor itself is a full vim implementation — `i`, `Esc`, `hjkl`, `w`,
`b`, `dd`, `yy`, `p`, `u`, `Ctrl+R`, visual mode, search, etc. See
[edtui's keymap](https://github.com/preiter93/edtui#keybindings) for the
complete list.

### Confirm-run prompt

After `<Space>r` in Normal mode, the editor shows a highlight over the
statement and the bottom bar reads:
`▶ run highlighted statement?  Enter to confirm · Esc to cancel`

| Keys     | Action            |
|----------|-------------------|
| `Enter`  | Run the statement |
| `Esc`    | Cancel            |

All other keys are intentionally ignored to prevent accidental edits.

### Schema panel

When focused (`Ctrl+W l`).

| Keys           | Action                                                |
|----------------|-------------------------------------------------------|
| `j` / `k`      | Move selection down / up                              |
| `h`            | Collapse node, or move to parent if already collapsed |
| `l`            | Expand node (loads on first expand), or descend       |
| `o` / `Enter`  | Toggle expand / collapse                              |
| `gg`           | Jump to top                                           |
| `G`            | Jump to bottom                                        |
| `<` / `>`      | Grow / shrink the panel width                         |

Nodes show their load state inline:
- `(loading…)` while a request is in flight
- `(error: …)` and a red label on a failed load — press `l`/`Enter` to retry

### Expanded result view

When you've expanded a result block (`<Space>e` or `:expand`).

| Keys         | Action                       |
|--------------|------------------------------|
| `h j k l`    | Move cell cursor             |
| `0` / `$`    | First / last column in row   |
| `gg` / `G`   | First / last row             |
| `q` / `Esc`  | Close expanded view          |

When the result has more columns than fit on screen the view scrolls
horizontally to keep the cursor visible. The title shows `cols X-Y of Z`
with `‹`/`›` markers when there are columns off-screen on either side. The
inline preview shows only the leftmost columns that fit and a `+N →` count
of how many were truncated — expand it to navigate.

### Command prompt

After pressing `:`.

| Keys             | Action                  |
|------------------|-------------------------|
| `Enter`          | Submit                  |
| `Esc`            | Cancel                  |
| `Backspace`      | Delete character        |
| `Left` / `Right` | Move cursor             |
| typing           | Insert character        |

## Commands

| Command                      | Effect                                                          |
|------------------------------|-----------------------------------------------------------------|
| `:q`, `:quit`                | Quit                                                            |
| `:run`, `:r`                 | Run the statement under the cursor (no confirmation)            |
| `:cancel`                    | Cancel the in-flight query                                      |
| `:expand`, `:e`              | Expand the latest result                                        |
| `:collapse`, `:c`            | Close the expanded result view                                  |
| `:width <cols>`              | Set schema panel width (clamped 12–80)                          |
| `:theme dark` \| `light`     | Switch theme                                                    |
| `:theme toggle` \| `:theme`  | Flip between Dark and Light                                     |
| `:conn`, `:conn list`        | Open the connection list                                        |
| `:conn add <name>`           | Open the form to create `<name>`                                |
| `:conn edit <name>`          | Open the form pre-filled with `<name>`'s URL (overwrite on save) |
| `:conn delete <name>`        | Remove `<name>` (refuses if it's the active connection)         |
| `:conn use <name>`           | Switch the active connection live                               |

### Connection list

Opened via `:conn`. Browseable with vim keys; the active connection is
marked with `●`.

| Keys           | Action                                                |
|----------------|-------------------------------------------------------|
| `j` / `k`      | Move selection                                        |
| `g` / `G`      | Jump to top / bottom                                  |
| `Enter` / `u`  | Switch to the selected connection                     |
| `a`            | Add a new connection (form opens)                     |
| `e`            | Edit the selected (form opens, pre-filled)            |
| `d`            | Delete the selected (`y`/`Enter` confirms, `n`/`Esc`) |
| `Esc` / `q`    | Close the list                                        |

## Project layout

```
src/
  main.rs                 async entry point + tokio::select event loop
  app.rs                  App state + cmd_tx handle to the worker
  action.rs               Action enum, apply() dispatcher, command parser
  event.rs                crossterm Event → Action translation
  cli.rs                  clap arg parsing (--connection NAME, --password)
  crypto.rs               argon2id KDF + chacha20poly1305 AEAD primitives
  connections.rs          ConnectionStore: encrypt/decrypt, unlock, make_entry
  config.rs               .rowdy/config.toml load + lazy save
  log.rs                  Logger — Arc<Mutex<File>>, info/warn/error
  subcommands.rs          non-TUI `rowdy connections …` handlers
  terminal.rs             terminal init / restore / panic hook
  state/                  sub-state modules
    editor.rs             EditorPanel + statement-under-cursor parser
    schema.rs             SchemaPanel + LoadState + tree population
    results.rs            ResultBlock + ResultCursor
    command.rs            CommandBuffer
    focus.rs              Focus + Mode + PendingChord
    status.rs             QueryStatus
    auth.rs               AuthState (password buffer + attempt counter)
    conn_form.rs          ConnFormState (name + url two-field form)
    conn_list.rs          ConnListState (saved connections, with delete-confirm)
  datasource/
    mod.rs                Datasource trait + connect() factory
    cell.rs               typed Cell enum + display helpers
    schema.rs             CatalogInfo / SchemaInfo / TableInfo / …
    error.rs              DatasourceError
    sql/
      sqlite.rs           SqliteDatasource (sqlx)
      postgres.rs         PostgresDatasource (sqlx)
      mysql.rs            MysqlDatasource (sqlx, also handles mariadb://)
  worker/
    mod.rs                tokio worker task, command/event channels
    request.rs            RequestId newtype + counter
  ui/
    mod.rs                render() — layout + cursor placement
    editor_view.rs        edtui rendering with themed block + highlights
    schema_view.rs        tree + load-state glyphs
    results_view.rs       inline preview + expanded grid
    auth_view.rs          centered password prompt
    conn_form_view.rs     centered name+url form
    conn_list_view.rs     centered connection picker
    bottom_bar.rs         status / command / confirm-run prompt
    theme.rs              Dark + Light palettes
examples/
  seed_sqlite.rs          creates a sample SQLite DB to test against
```

## Roadmap

Next likely steps, roughly ordered:

- **Real cancel** for Postgres (`pg_cancel_backend(pid)`) and MySQL
  (`KILL QUERY <id>`) — needs the worker to track the backend PID /
  connection id of the in-flight query.
- **`NUMERIC` / `DECIMAL`** decoding for Postgres and MySQL — currently
  falls through to `Cell::Other`. Wiring sqlx's `bigdecimal` feature would
  fix it.
- **Export**: `:export <path>` for CSV / TSV / JSON / SQL inserts. The
  typed `Cell` model is already in place to support this without losing
  fidelity.
- **Multiple result blocks** stacked under the editor with scrolling
  (currently only the latest is shown).
- **A real SQL lexer** for statement splitting (the current `;` splitter is
  intentionally naive — see the TODO at `state/editor.rs`).
- **Elapsed-time** rendering for in-flight queries
  (`QueryStatus::Running.started_at` is already captured).
- **Query history** surfaced under each result block
  (`ResultBlock.query` is already captured).
- **SQL syntax highlighting** in the editor.
- **Theme persistence** via a config file.
