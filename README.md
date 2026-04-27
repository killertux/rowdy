# rowdy

A terminal SQL client built with [ratatui](https://github.com/ratatui/ratatui),
[edtui](https://github.com/preiter93/edtui), and [sqlx](https://github.com/launchbadge/sqlx).
The goal is a fast, modal, keyboard-first workspace for writing queries,
exploring schemas, and inspecting results — all without leaving the terminal.

SQLite, Postgres, and MySQL/MariaDB are wired end-to-end. Most authoring
features (autocomplete, formatting, yank, CSV/TSV/JSON/SQL export) are
shipped; the rough edges live around transactions and multi-statement
execution — see [Roadmap](#roadmap).

## Install

The install script grabs the latest GitHub Release artifact for your
OS/arch and drops the binary in `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/killertux/rowdy/main/install.sh | sh
```

Supported targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`aarch64-apple-darwin` (Apple Silicon). macOS Intel and Windows aren't
covered — build from source instead.

Override via env:

- `ROWDY_INSTALL_DIR=/usr/local/bin` — different install location
- `ROWDY_VERSION=v0.1.0` — pin a specific release (default: latest)

If the install dir isn't already on your `$PATH`, the script prints the
line to add to your shell rc.

### Build from source

Requires Rust 2024 edition (≥ 1.85) and a terminal that supports truecolor
for accurate theme rendering.

```sh
git clone https://github.com/killertux/rowdy
cd rowdy
cargo build --release
# binary at ./target/release/rowdy
```

## Running it

Point rowdy at any database via a connection URL:

```sh
rowdy --connection sqlite:./sample.db
```

Or run straight from a checkout:

```sh
cargo run -- --connection sqlite:./sample.db
```

### Sample database

A seed program creates a sample SQLite database to poke at: a small
e-commerce schema (`users` / `products` / `orders` / `order_items` plus a
`recent_orders` view), an `events` table with 5000 rows, a `wide_metrics`
table with 32 columns to exercise horizontal scroll, and 10 small lookup
tables to exercise the schema panel's vertical scroll.

```sh
cargo run --example seed_sqlite -- ./sample.db
cargo run -- --connection sqlite:./sample.db
```

Re-running the seeder is safe — it drops and re-creates the tables.

### Saved connections

You can save connection URLs (optionally encrypted) and switch between
them inside the TUI. Open the connection list with `:conn`, or manage
them without launching the TUI:

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

- `config.toml` — theme, schema-panel width, saved connections, and (when
  the encrypted store is in use) the argon2id/chacha20-poly1305 crypto
  block. Written **lazily** on the first change away from defaults.
- `<datetime>.log` — one file per session, named for the launch time.
  Append-only. The app and every datasource log into it (connect /
  execute / cancel / errors / session save+load). URL passwords are
  redacted. Only the 5 most recent log files are kept; older ones are
  deleted at the start of the next launch.
- `sessions/<connection-name>/session_0.sql` — the editor buffer for each
  saved connection. Auto-saved 800ms after the last edit and reloaded on
  the next connect. Connection names are sanitised for path safety, so
  two names that differ only in path-unsafe characters share a session
  for now.

## Connection strings

URL scheme dispatches to the driver:

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

## Features

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
  Timestamp / Date / Time / Uuid / Other`) — preserved end-to-end so the
  CSV / TSV / JSON / SQL exporters keep type fidelity. The TUI renders each
  cell via its own `display()`; `NULL` cells are dimmed. The bottom row of
  the expanded result view shows the full value of the selected cell so
  wide values stay readable when the column is narrower than them.
- **Yank and export** in the expanded result view: `y` copies the
  current cell to the clipboard, `v` enters Visual mode for a
  rectangular selection, and `:export csv|tsv|json|sql` (or `y` from
  Visual) copies the result — full or selected — in the chosen format.
  `:export sql` infers the source table from simple `SELECT … FROM t`
  shapes; pass it explicitly for joins/aggregates. Pass a path after the
  format to write to disk instead of the clipboard.
- **SQL autocomplete** — syntax-aware popover via sqlparser's tokenizer.
  Completes keywords, tables, columns, SQL functions, and CTE names with
  FROM/JOIN alias resolution. Auto-triggers in Insert mode after `.` or
  2+ identifier chars (`Ctrl+Space` forces it). Fuzzy-ranked, kind-boosted,
  dialect-quoted on insert. Schema cache primed at connect time; columns
  load lazily; DDL run from rowdy auto-reloads. See the
  [Autocomplete](#autocomplete) reference for the full behaviour.
- **SQL formatter** — `=` in editor Normal mode formats the whole buffer;
  in Visual mode formats the selection. `:format` does the same.
- **Three SQL drivers** sharing the same `Datasource` trait:
  - **SQLite** — in-memory or file-based, schema via `sqlite_master` and
    `pragma_*` virtual tables.
  - **Postgres** — schema via `pg_namespace` + `information_schema`, indices
    via `pg_class`/`pg_index` for the uniqueness flag. User-defined `ENUM`
    types decode to their variant string.
  - **MySQL / MariaDB** — schema via `information_schema`, `column_type` for
    declared types (preserves `unsigned`, display widths, etc.).
- **Two themes** (Dark / Light) switchable at runtime, both tuned for high
  text contrast. Theme + schema-panel width persist to `./.rowdy/config.toml`.
- **Saved connections** in `./.rowdy/config.toml`, optionally encrypted with a
  password (argon2id + chacha20-poly1305). Pick one from `:conn`, switch live
  with `:conn use NAME`. The password prompts in-TUI on launch or via
  `--password`. Manage them without the TUI via `rowdy connections …`.
- **Per-connection editor sessions** persisted at
  `./.rowdy/sessions/<name>/session_0.sql`. The buffer is flushed 800ms
  after the last edit and reloaded on the next launch (or `:conn use`
  switch).
- **Vim-style modal input** end-to-end: editor uses real vim bindings via
  edtui; the schema panel and result viewer use the same `hjkl` / `gg` / `G`
  vocabulary.
- **File logger** at `./.rowdy/<datetime>.log` — connect / execute / cancel /
  errors. URL passwords are redacted; only the 5 most recent logs are kept.

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
| `=`         | Format SQL (Visual: selection; Normal: whole buffer)            |
| `Ctrl+Space`| Open SQL autocomplete popover (works in any editor mode)        |

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

| Keys         | Action                                                     |
|--------------|------------------------------------------------------------|
| `h j k l`    | Move cell cursor                                           |
| `0` / `$`    | First / last column in row                                 |
| `gg` / `G`   | First / last row                                           |
| `y`          | Yank — current cell (Normal) or selection (Visual, prompts for format) |
| `v`          | Toggle Visual mode (rectangular cell selection)            |
| `q` / `Esc`  | Visual: exit Visual · Normal: close expanded view          |

When the result has more columns than fit on screen the view scrolls
horizontally to keep the cursor visible. The title shows `cols X-Y of Z`
with `‹`/`›` markers when there are columns off-screen on either side. The
inline preview shows only the leftmost columns that fit and a `+N →` count
of how many were truncated — expand it to navigate.

The bottom row of the expanded view shows `column: value` for the selected
cell, clipped with `…` when the value is wider than the row. Use yank to
get at the full text when the badge is clipped.

#### Yank and export

`y` in Normal sub-mode copies the current cell's rendered text straight
to the system clipboard — no header, no quoting.

`y` in Visual sub-mode opens a tiny prompt at the bottom of the screen:
`yank as: [c]sv [t]sv [j]son [s]ql · Esc cancel`. A single key picks the
format and the selection is copied; `Esc` returns you to Visual with the
selection intact.

`:export csv|tsv|json|sql` does the same thing from the command bar.
With an active Visual selection it exports just the rectangle; otherwise
it exports the latest result block in full.

Pass a path after the format to write to disk instead of the clipboard:
`:export csv path/to/out.csv`. A leading `>` is optional and ignored
(`:export csv > out.csv` is the same call). `~` and `~/` expand to
`$HOME`; everything else is passed verbatim to the OS. The parent
directory must already exist; existing files are overwritten without a
prompt.

Format details:
- **CSV** — RFC 4180. Fields with commas, quotes, or newlines are quoted;
  internal `"` is doubled; `NULL` becomes an empty field.
- **TSV** — tabs separate fields; tabs / newlines / carriage returns
  inside a cell are replaced with spaces so the table shape survives a
  paste into a spreadsheet. Use CSV if you need exact round-trip.
- **JSON** — `[{column: value, …}, …]`. `Bool` / `Int` / `UInt` / `Float`
  cells become native JSON values, `Null` becomes `null`, bytes render
  as a hex string (`"0xdeadbeef"`), and `NUMERIC` / `DECIMAL` come
  through as JSON strings (preserves precision; round-trips into
  `BigDecimal::from_str`). Everything else is a string. NaN / infinity
  floats fall through to `null`.
- **SQL** — multi-row `INSERT INTO <table> (cols) VALUES (...);`,
  chunked at 100 rows per statement. Identifiers are dialect-quoted
  (`"x"` for SQLite/Postgres, `` `x` `` for MySQL); strings double
  internal `'`; bytes render as `X'…'` for SQLite/MySQL or
  `'\x…'::bytea` for Postgres; SQLite booleans become `1`/`0`.
  - **Source-table inference**. `:export sql` (no table) parses the
    originating query and accepts: a single bare-table `FROM` (no
    JOIN/CTE/subquery) plus a projection that's either a pure
    wildcard (`*` or `<table>.*`) or a list of bare/qualified
    identifiers without aliases. Anything else (joins, aggregates,
    aliased projections, computed columns) refuses inference and
    asks for `:export sql <table>`. Visual selection only requires
    the *selected* projection items to satisfy the rule, so a
    column-subset of a join can still infer if those particular
    columns are clean.
  - **Limitations**. No `CREATE TABLE` prelude (target schema must
    already exist), no `BEGIN`/`COMMIT` wrapping, no `ON CONFLICT` /
    `ON DUPLICATE KEY` clauses; selecting a column subset that
    excludes `NOT NULL` columns won't round-trip cleanly.

### Autocomplete

The popover **auto-opens** in editor Insert mode after you type `.` or
two identifier characters; `Ctrl+Space` forces it open in any editor
mode. Selection and acceptance:

| Keys                  | Action                                          |
|-----------------------|-------------------------------------------------|
| `Up`, `Ctrl+P`        | Previous candidate                              |
| `Down`, `Ctrl+N`      | Next candidate                                  |
| `Enter`, `Tab`        | Accept the highlighted candidate                |
| `Esc`                 | Close the popover (and snooze auto-trigger here) |

While the popover is open you can keep typing — each keystroke
re-filters the candidate list. Pressing `Esc` snoozes auto-trigger for
the current word; move the cursor or start a new word to re-enable it.
`Ctrl+Space` always opens regardless of snooze.

**Context awareness.** Tokens around the cursor determine the
suggestions:

- After `FROM` / `JOIN` / `INTO` / `UPDATE` / `TABLE` → **tables** in
  the default schema (or the named schema after `<schema>.`), plus
  any **CTE names** declared with `WITH`.
- After `<alias>.` or `<table>.` → **columns** of that bound table.
  Bound CTEs return no columns yet.
- After `SELECT` / `WHERE` / `ON` / `AND` / `,` / operators →
  **columns** unioned across FROM/JOIN bindings (qualifier-free)
  plus **SQL functions** (per-dialect curated list).
- Statement start, after `;`, or unrecognised slot → **keywords**.

FROM/JOIN aliases are resolved by a forward pass over the *whole*
statement, so `SELECT u.|` autocompletes correctly even when the
`FROM users u` clause comes after the cursor. CTE definitions
(`WITH name AS (…)`, optional `RECURSIVE`, multiple comma-separated
CTEs) are recognized too; subqueries and derived-table aliases are
not yet bound.

**Ranking.** Candidates are scored with `nucleo-matcher` (fuzzy
subsequence match) and re-ordered by:

1. Score (higher = better, with a +500 bonus when the label has the
   user's exact prefix).
2. **Kind bonus** matched to the syntactic context: `+1000` for
   columns in column slots, tables in table slots, etc., so a
   shorter coincidentally-matching keyword can't shadow the right
   answer.
3. Shorter labels first.
4. Alphabetical.

**Insert.** Accepting an item writes into the buffer with three
refinements:

- **Quoting** when the identifier can't sit unquoted: any uppercase
  char (Postgres folds unquoted to lowercase), any non-`[A-Za-z0-9_]`
  char, leading digit, or one of the curated reserved keywords. The
  quote style follows the dialect — `"x"` for SQLite/Postgres,
  `` `x` `` for MySQL — and any internal quote chars are doubled.
  Keywords and functions are inserted *as displayed*, never quoted.
- **Trail** depends on item kind:
  - **Table / view** in a FROM/JOIN slot → trailing space.
  - **Function** with arguments → appended `()` with the cursor
    between them, ready for arguments.
  - **Function** with no arguments (`NOW`, `CURRENT_TIMESTAMP`,
    `CURDATE`, …) → appended `()`, cursor at the end.
  - **Column / keyword / CTE** → no trail.

**Schema cache.** Catalogs, schemas of the default catalog, and tables
of the default schema are eagerly loaded on connect. **Columns load
lazily** the first time you reference a table; the popover shows a
"loading…" placeholder briefly and refreshes when the data arrives.
DDL statements (`CREATE`, `ALTER`, `DROP`, `TRUNCATE`, `RENAME`)
executed from rowdy auto-reload the cache. For schema changes made
outside rowdy, run `:reload` to re-prime.

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
| `:help`, `:?`                | Open the help popover (bindings + commands)                     |
| `:run`, `:r`                 | Run the statement under the cursor (no confirmation)            |
| `:cancel`                    | Cancel the in-flight query                                      |
| `:expand`, `:e`              | Expand the latest result                                        |
| `:collapse`, `:c`            | Close the expanded result view                                  |
| `:width <cols>`              | Set schema panel width (clamped 12–80)                          |
| `:theme dark` \| `light`     | Switch theme                                                    |
| `:theme toggle` \| `:theme`  | Flip between Dark and Light                                     |
| `:export csv` \| `tsv` \| `json` `[path]` | Copy the latest result (or Visual selection) to the clipboard, or write to `path` if given |
| `:export sql [table] [path]` | Emit `INSERT` statements. Table is inferred from the query for simple `SELECT * FROM t` / `SELECT cols FROM t` shapes; pass `<table>` explicitly for joins, aggregates, aliases, etc. `:export sql > path` writes to disk with inferred table |
| `:format`, `:fmt`            | Format the SQL buffer (or active Visual selection) via `sqlformat`. Undo via edtui's `u` won't restore the pre-format text — yank first if you need a backup |
| `:reload`                    | Drop and re-prime the autocomplete schema cache against the active connection (use after DDL outside the app) |
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

### Postgres / MySQL test databases

The Postgres and MySQL drivers have integration tests gated on
`ROWDY_POSTGRES_URL` and `ROWDY_MYSQL_URL` — when either is unset the
test prints a skip notice and returns Ok, so `cargo test` is green on a
machine without those databases. To exercise them locally:

```sh
docker compose up -d
ROWDY_POSTGRES_URL=postgres://rowdy:rowdy@localhost:55432/rowdy_test \
ROWDY_MYSQL_URL=mysql://rowdy:rowdy@localhost:53306/rowdy_test \
cargo test
```

The non-default ports (`55432` / `53306`) are deliberate so they don't
collide with a system Postgres / MySQL on the standard ports. CI starts
the same images via GitHub Actions `services` and uses the standard
ports there.

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
  most one query at a time, and fans introspection out concurrently.
  `:cancel` aborts the in-flight `JoinHandle` *and* sends a server-side
  cancel (`pg_cancel_backend` for Postgres, `KILL QUERY` for MySQL) so the
  database doesn't keep grinding on a query the user gave up on. SQLite
  has no server-side cancel; the abort is the cancel.

State is encoded so that invalid combinations are unrepresentable wherever
possible:

- `Focus { Editor, Schema }` — exactly one panel owns input.
- `Mode { Normal, Command(CommandBuffer), ResultExpanded { id, cursor,
  col_offset, row_offset }, ConfirmRun { statement }, Auth(AuthState),
  EditConnection(ConnFormState), ConnectionList(ConnListState),
  Connecting { name } }` — every variant carries the data its UI needs;
  no "expanded but no result", no "in command mode but no buffer", no
  "awaiting confirmation but no statement".
- `QueryStatus { Idle, Running, Succeeded, Failed, Cancelled }` — replaces a
  bag of booleans / `Option<String>` fields.
- `LoadState { NotLoaded, Loading, Loaded, Failed(error) }` on every schema
  node — drives the lazy-load UX without any "is_loading + error" pairs.
- `IntrospectTarget` — a single value identifies both *which level* to load
  and *which DB entity* it belongs to, so worker events reattach to the
  right node deterministically.

## Project layout

```
src/
  main.rs                 async entry point + tokio::select event loop
  app.rs                  App state + cmd_tx handle to the worker
  action.rs               Action enum, apply() dispatcher, command parser
  event.rs                crossterm Event → Action translation
  cli.rs                  clap arg parsing (--connection NAME, --password)
  clipboard.rs            arboard wrapper for paste/copy/cut into inputs
  crypto.rs               argon2id KDF + chacha20poly1305 AEAD primitives
  connections.rs          ConnectionStore: encrypt/decrypt, unlock, make_entry
  config.rs               .rowdy/config.toml load + lazy save
  log.rs                  Logger — Arc<Mutex<File>>, info/warn/error
  export.rs               CSV / TSV / JSON / SQL formatters for yank + :export
  session.rs              .rowdy/sessions/<name>/session_0.sql load + save
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
    results_view.rs       inline preview + expanded grid + cell badge
    auth_view.rs          centered password prompt
    conn_form_view.rs     centered name+url form
    conn_list_view.rs     centered connection picker
    help_view.rs          `:help` popover (bindings + commands cheat sheet)
    bottom_bar.rs         status / command / confirm-run prompt
    theme.rs              Dark + Light palettes
examples/
  seed_sqlite.rs          creates a sample SQLite DB to test against
```

## Roadmap

Next likely steps, roughly ordered:

### Correctness / safety

- **Transactions.** `Datasource::execute` runs every statement on
  `&self.pool`, so sqlx hands each call a fresh pooled connection — `BEGIN`
  lands on connection A and the next `UPDATE` may land on connection B.
  The trait needs a transaction handle (or a "stick the next N statements
  to one connection" mode) before BEGIN/COMMIT/ROLLBACK behave the way the
  user expects.
- **Multi-statement execution.** `:run` runs the statement under the
  cursor; there's no way to run a buffer of N statements as a unit. Pairs
  with the real-SQL-lexer item below — splitting is necessary but not
  sufficient, the execution model also needs to pin one connection for
  the duration of the script.
- **Query timeout** per connection. A runaway query holds the worker's
  one-at-a-time slot until the user manually `:cancel`s; a default
  timeout (with the existing server-side cancel path) would be a cheap
  guardrail.

### Authoring

- **Autocomplete — bound subqueries / derived tables.** Currently
  FROM/JOIN aliases against base tables and CTE names resolve, but
  CTE *bodies* and `FROM (SELECT …) sub` derived tables don't yield
  column completions. Parsing those into a binding scope is the next
  step; auto-alias suggestion and configurable trigger thresholds
  are smaller follow-ups.
- **A real SQL lexer** for statement splitting (the current `;` splitter is
  intentionally naive — see the TODO at `state/editor.rs`).
- **Multiple sessions per connection.** Each connection has a single
  `session_0.sql` buffer today — picking a connection swaps the editor
  to that one buffer. A tabbed model (`session_0.sql`, `session_1.sql`,
  …) would let users keep a long-running migration draft separate from
  ad-hoc queries against the same database.

### Result view

- **Cell zoom / detail view** for long TEXT / JSON cells. The bottom-row
  badge shows the full single-line value for the selected cell, but
  multi-line or very long values still need a scrollable modal — open
  it with `Enter` (or similar) on a cell in the expanded view.
- **Multiple result blocks** stacked under the editor with scrolling
  (currently only the latest is shown).
- **Query history** surfaced under each result block.
- **`:explain` / `<Space>x`** that wraps the statement under the cursor
  in `EXPLAIN` (or `EXPLAIN ANALYZE`) for the active dialect.

### Connection management

- **"Test connection"** action in the connection form — fire a one-shot
  connect-and-disconnect so URL typos surface before the user saves and
  switches.
