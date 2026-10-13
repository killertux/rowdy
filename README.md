# rowdy

A terminal SQL client built with [ratatui](https://github.com/ratatui/ratatui),
[edtui](https://github.com/preiter93/edtui), and [sqlx](https://github.com/launchbadge/sqlx).
The goal is a fast, modal, keyboard-first workspace for writing queries,
exploring schemas, and inspecting results — all without leaving the terminal.

SQLite, Postgres, MySQL/MariaDB, and [Stoolap](https://github.com/stoolap/stoolap)
are wired end-to-end. Most authoring
features (autocomplete, formatting, vim search & replace, query
parameters, saved queries, yank, CSV/TSV/JSON/SQL export, transactions
across statements, an LLM chat companion, 14 themes) are shipped — see
[Roadmap](#roadmap) for what's still ahead.

![rowdy: SQL editor, inline result preview, and the lazily-loaded schema tree](docs/screenshots/overview.png)

<sub>Screenshots are generated from the sample database with
[vhs](https://github.com/charmbracelet/vhs) — see
[`docs/screenshots/generate.sh`](docs/screenshots/generate.sh).</sub>

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

### Auto-update

When rowdy starts, it asynchronously checks GitHub for a newer release
(at most once every 24 hours, with no impact on launch time). If a new
version is available, the bottom bar prompts:

```
⬆ rowdy 0.6.2 → v0.7.0 available  y to update · n/Esc to dismiss
```

The prompt waits until you're back on the main screen and not typing.
Pressing `y` (or `Enter`) re-runs the install script, pinned to the
offered release, against the running binary's directory (so it
overwrites the rowdy you're currently using; needs `curl`). After a
successful install, restart rowdy to pick up the new binary. Pressing
`n` records the dismissed version in `~/.rowdy/config.toml` so we don't
re-prompt for the same release on every launch.

To disable the check entirely, add to `~/.rowdy/config.toml`:

```toml
check_for_updates = false
```

`:update` runs the check on demand — it ignores the 24-hour throttle,
a previously dismissed version, and `check_for_updates = false`.

### Build from source

Requires Rust 2024 edition (≥ 1.88 — the code uses let-chains) and a
terminal that supports truecolor for accurate theme rendering.

```sh
git clone https://github.com/killertux/rowdy
cd rowdy
cargo build --release
# binary at ./target/release/rowdy
```

## Running it

rowdy opens **saved connections** by name. On first launch (no saved
connections yet) just run `rowdy` — it opens the connection form, where
you give the connection a name and a URL (`Ctrl+T` tests it before
saving). After that:

```sh
rowdy                       # pick from the saved connections
rowdy -c local              # open the saved connection named "local"
rowdy -c local -p secret    # …and unlock an encrypted store non-interactively
```

`-c/--connection` takes a connection **name**, not a URL. To save one
from the shell instead of the TUI, see
[Saved connections](#saved-connections). From a checkout, replace
`rowdy` with `cargo run --`.

### Sample database

A seed program creates a sample SQLite database to poke at: a small
e-commerce schema (`users` / `products` / `orders` / `order_items` plus a
`recent_orders` view), an `events` table with 5000 rows, a `wide_metrics`
table with 35 columns to exercise horizontal scroll, and 10 small lookup
tables to exercise the schema panel's vertical scroll.

```sh
cargo run --example seed_sqlite -- ./sample.db
cargo run -- connections add sample --url sqlite:./sample.db --password ""
cargo run -- -c sample
```

Re-running the seeder is safe — it drops and re-creates the tables.

### Saved connections

You can save connection URLs (optionally encrypted) and switch between
them inside the TUI. Open the connection list with `:conn`, or manage
them without launching the TUI:

```sh
rowdy connections list                                        # alias: ls
rowdy connections add <name> --url <url> [--password <pw>]
rowdy connections edit <name> --url <url> [--password <pw>]   # overwrite
rowdy connections delete <name>                               # alias: rm
```

Password handling for `add` / `edit`:

- **Flag absent** — prompts on stdin (masked) when the store is
  encrypted or still empty; an existing plaintext store isn't prompted.
  `rpassword` falls back to reading from a pipe if stdin isn't a TTY.
- **`--password X`** (non-empty) — uses `X`. On a store without a crypto
  block yet this also initialises it (existing plaintext entries are
  *not* re-encrypted, so start from an empty store if you want
  everything encrypted).
- **`--password ""`** — explicit "no encryption". Only valid against an empty
  store or an existing plaintext store; refused against an encrypted one.
  (When launching the TUI, `--password ""` is treated as if the flag were
  absent.)

`list` and `delete` never touch the password — they're pure config edits.

### Per-directory state: `.rowdy/`

On startup rowdy creates `.rowdy/` in the current working directory if it
doesn't exist. It holds:

- `config.toml` — theme, schema-panel width, saved connections, LLM
  provider records, and (when the encrypted store is in use) the
  argon2id/chacha20-poly1305 crypto block. Written **lazily** on the
  first change away from defaults.
- `<datetime>.log` — one file per TUI session, named for the local launch
  time (`YYYY-MM-DD_HH-MM-SS.log`). Append-only. The app and every
  datasource log into it (connect / execute / cancel / errors / session
  save+load). URL passwords are redacted. Only the 5 most recent log
  files (including the new one) are kept; older ones are pruned at
  launch. `rowdy connections …` doesn't create a log.
- `sessions/<connection-name>/session_<N>.sql` — editor buffers for
  each saved connection, indexed from `0`. Each connection starts with
  `session_0.sql`; create more with `:session new` and switch between
  them with `:session next`/`prev`/`<N>` or `<Space>n`. The active
  buffer is auto-saved 800ms after the last edit and reloaded on the
  next connect (or session switch). Indices may have holes after
  deletion (`session_0.sql` + `session_2.sql` is fine without a
  `session_1.sql`). Connection names are sanitised for path safety,
  so two names that differ only in path-unsafe characters share a
  sessions directory for now.
- `chats/<connection-name>/session.jsonl` — the LLM chat history for
  each saved connection. Append-only JSONL; one `ChatMessage` per line.
  System messages are filtered out on append so we don't persist tool
  prompts. Wiped by `:chat clear`.
- `queries/<connection-name>/<name>.sql` — named saved queries
  (`:save` / `:load` / `:run-saved`, see [Saved queries](#saved-queries)).
- `params/<connection-name>.json` — the last values you typed into the
  [query parameter](#query-parameters) prompt, capped at the 200 most
  recently used.

### User configuration: `~/.rowdy/`

Rowdy also reads from `$HOME/.rowdy/` (Unix) or
`%USERPROFILE%\.rowdy\` (Windows) for cross-project defaults. You
create it yourself for preferences; rowdy only writes to it for its
own bookkeeping (the update-check timestamp, a dismissed update, and
the chat read-tools mode).

- `config.toml` — user-level settings, all optional:

  | Key                            | Default       | Meaning                                                      |
  |--------------------------------|---------------|--------------------------------------------------------------|
  | `theme`                        | `base16-dark` | Default theme (see [Themes](#themes))                        |
  | `schema_width`                 | —             | Default schema-panel width                                   |
  | `check_for_updates`            | `true`        | Background release check on launch                           |
  | `completion_trigger_threshold` | `2`           | Identifier chars before autocomplete auto-opens (`0` = only after `.`) |
  | `read_tools`                   | `ask`         | Chat file-tool policy: `off` / `ask` / `auto`                |

  `last_update_check_at` and `last_dismissed_version` are written by
  rowdy. Project-level `./.rowdy/config.toml` overrides `theme` and
  `schema_width` per-field on read, so per-project pins still win.
- `keybindings.toml` — sparse override of the default keymap. Only
  the chords you change need to be listed; everything else stays
  on its default.

Connections, the encrypted crypto block, LLM providers and API keys,
per-session editor buffers, saved queries, parameter history, chat
logs, and the on-disk session log all stay **per-project** in
`./.rowdy/`. They are not user-wide.

Runtime mutators for UI prefs (`:theme <name>`, `:width 48`) write to
the **project** file, not the user file — the project file remains the
deliberate-pin lever.

#### `:source`

`:source` re-reads `~/.rowdy/config.toml`, `./.rowdy/config.toml`
(re-applying theme + schema_width), and `~/.rowdy/keybindings.toml`,
and reloads `AGENTS.md` for the chat. The active connection, the
unlocked crypto store, the worker pool, and any in-flight query are
**not** touched. On a malformed `keybindings.toml` the previous
keymap stays active — you don't lose your working overrides because
of one typo. A malformed `config.toml` aborts the reload.

#### `keybindings.toml` worked example

```toml
# Sparse override — only the chords you change need to appear.
# Defaults that aren't listed stay on their built-in chord.

[leader]
# `<Space>r` cancels the running query instead of prompting to run
# the statement under the cursor.
r = "cancel-query"

[global_immediate]
# `=` opens the autocomplete popover instead of formatting.
"=" = "open-completion-popover"

[schema]
# `o` collapses-or-ascends instead of toggling.
o = "schema-collapse-or-ascend"
```

Tables are keyed by **context** (`global_immediate`, `leader`,
`schema`, `result`, `chat_normal`, `chat_insert`). Inside each table,
keys are chord-notation strings and values are bindable action IDs.

**Chord notation.** Bare characters (`r`, `:`, `0`, `$`), named
keys (`<Esc>`, `<Enter>`/`<Return>`, `<Tab>`, `<Up>`, `<Down>`,
`<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`, `<PageDown>`,
`<Space>`, `<BackTab>`, `<Backspace>`/`<BS>`), and modifier prefixes
(`<C-x>`, `<A-x>`, `<S-r>`, `<C-S-r>`, `<C-Space>`). Each entry is a
**single key** looked up inside its context table — in `[leader]`
write `r`, not `<Space>r`. Two-key sequences such as `gg` or
`<Space>r` are rejected with an error.

**Chord openers are NOT rebindable.** `<Space>` (leader chord),
`Ctrl+W` (window chord), and `g` (GG navigation) are intercepted before
the keymap is consulted, so binding them in any context table has no
effect. Rebind the keys *under* them (the `[leader]` table) instead.

**All-or-nothing.** If any entry has an unknown chord or action ID (or is a multi-key sequence),
the whole file is rejected and the defaults (or, on `:source`, your
previous keymap) stay active. Overrides add bindings; there is no way
to unbind a default.

**Bindable action IDs by context.**

| Context           | Action ID                       | Description                                                |
|-------------------|---------------------------------|------------------------------------------------------------|
| `global_immediate`| `open-command`                  | Open command prompt                                        |
| `global_immediate`| `format-buffer`                 | Format SQL under the cursor                                |
| `global_immediate`| `grow-schema`                   | Grow schema panel width                                    |
| `global_immediate`| `shrink-schema`                 | Shrink schema panel width                                  |
| `global_immediate`| `open-completion-popover`       | Open autocomplete popover (no default chord — `Ctrl+Space` is built in) |
| `leader`          | `run-prompt-or-selection`       | Run selection (Visual) / prompt to run statement (Normal)  |
| `leader`          | `run-statement-under-cursor`    | Run the statement under the cursor — no prompt             |
| `leader`          | `cancel-query`                  | Cancel the in-flight query                                 |
| `leader`          | `expand-latest-result`          | Expand the latest result to full view                      |
| `leader`          | `set-right-panel-schema`        | Switch right panel to schema (and focus)                   |
| `leader`          | `set-right-panel-chat`          | Switch right panel to chat (and focus)                     |
| `leader`          | `next-session`                  | Cycle to the next per-connection editor session            |
| `leader`          | `session-switch-0` … `-9`       | Jump straight to session N (default chord: `<Space>` then the digit `0`–`9`) |
| `schema`          | `schema-up` / `schema-down`     | Move selection                                             |
| `schema`          | `schema-collapse-or-ascend`     | Collapse node or ascend                                    |
| `schema`          | `schema-expand-or-descend`      | Expand node or descend                                     |
| `schema`          | `schema-toggle`                 | Toggle expand / collapse                                   |
| `schema`          | `schema-bottom`                 | Jump to bottom                                             |
| `result`          | `result-{up,down,left,right}`   | Move cell cursor                                           |
| `result`          | `result-line-start` / `-end`    | First / last column in row                                 |
| `result`          | `result-bottom`                 | Last row                                                   |
| `result`          | `result-yank`                   | Yank selected cell or selection                            |
| `result`          | `result-column-{move-left,move-right,hide,reset}` | Column ops                              |
| `chat_normal`     | `chat-enter-insert`             | Focus the composer                                         |
| `chat_normal`     | `chat-{scroll-up,scroll-down}`  | Scroll log line by line                                    |
| `chat_normal`     | `chat-{page-up,page-down}`      | Scroll log by a page                                       |
| `chat_normal`     | `chat-{top,bottom}`             | Jump to top / bottom of log                                |
| `chat_insert`     | `chat-{scroll-up,scroll-down,page-up,page-down}` | Log scroll while composer is focused      |

All six tables are routed through the keymap inside `event::translate`.
In `result`, `chat_normal`, and `chat_insert` a few keys whose meaning
depends on per-mode sub-state stay built in and are checked first:
`Esc`, `q`, `v`, `:`, and `g` in the expanded result view; `Esc` and `g`
in chat Normal; clipboard keys, `Ctrl+U`, `Ctrl+W`, `Esc`, and `Enter`
in the chat composer. Built-in aliases that aren't in the keymap (arrow
keys, `Home`/`End`, `I`) keep working alongside your overrides. Leader
overrides show up in the `<Space>` hint popover; the `:help` cheat sheet
is static.

**Sparse overrides + new defaults.** Newly-added defaults reach
existing users automatically. If you happened to bind the same chord
to a different action, the new default is shadowed (your override
wins).

**`keybindings.toml` deletion** is a valid state — defaults go back
into effect at the next launch / `:source`. Rowdy does not error
when the file is missing.

## Connection strings

URL scheme dispatches to the driver:

| Scheme                       | Driver     | Example                                                |
|------------------------------|------------|--------------------------------------------------------|
| `sqlite:`                    | SQLite     | `sqlite:./sample.db`, `sqlite::memory:?cache=shared`   |
| `postgres:` / `postgresql:`  | Postgres   | `postgres://user:pass@host:5432/db`                    |
| `mysql:` / `mariadb:`        | MySQL      | `mysql://user:pass@host:3306/db`                       |
| `stoolap:`                   | Stoolap    | `stoolap:./data/mydb`, `stoolap::memory:`              |

`mariadb://` is rewritten to `mysql://` before sqlx sees it — same wire
protocol, same driver. `postgres://` and `postgresql://` are interchangeable.

Stoolap is embedded, so it runs in-process like SQLite. A `stoolap:` path
names a *directory* (created if missing); stoolap options go in the query
string (`stoolap:./data/mydb?sync_mode=full`), and a native stoolap DSN
after the prefix passes through (`stoolap:file:///abs/path`). Stoolap
locks the directory, so it can't be open in rowdy and another process at
the same time. It has no catalogs or schemas — tables appear under a
single `main`.

> **In-memory SQLite caveat:** the worker uses a connection pool, and each
> SQLite memory connection gets its *own* database unless you opt into
> shared cache. Use `sqlite::memory:?cache=shared` (or a file path) so
> introspection sees the data your queries created.

> **System schemas are hidden** by default — Postgres `pg_catalog`,
> `information_schema`, `pg_toast`, `pg_temp_*`, `pg_toast_temp_*`; MySQL `information_schema`,
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
- **Transaction-pinned connection.** Once you issue `BEGIN`, the driver
  pins that pool connection until the transaction ends, so
  `BEGIN` / `COMMIT` / `ROLLBACK` issued as separate statements all land
  on the same backend — an interactive transaction can span several
  `<Space>r` runs. Outside a transaction the connection goes back to
  the pool after each statement (so session state such as
  `SET search_path` isn't guaranteed to stick). Introspection and
  `:cancel` use the pool, so neither stalls behind a slow query.
  Postgres/MySQL pools connect lazily (max 3 connections, idle ones
  reaped after 60s).
- **Multi-statement execution.** A Visual selection (`<Space>r`) or a
  saved query containing several semicolon-separated statements runs
  them in order on the same session. The bottom bar shows
  `ok — N rows in T · ran K statements`. Execution stops at the first
  error (`statement K of N failed: …`) so a broken script doesn't keep
  mutating state past the failure.
- **Confirm-before-run**: `<Space>r` highlights the SQL statement under the
  cursor and asks before executing it. `<Space>R` bypasses the confirmation;
  in editor Visual mode `<Space>r` runs the explicit selection straight away.
- **Destructive-statement guard**: `UPDATE` / `DELETE` without a `WHERE`
  and any `TRUNCATE` always stop for a `⚠ … — confirm to run` prompt,
  even from `<Space>R`, `:run`, or a Visual selection.
- **Query parameters**: `$1` / `:name` placeholders open a prompt to fill
  them in before running — see [Query parameters](#query-parameters).
- **Saved queries** per connection: `:save`, `:load`, `:run-saved` — see
  [Saved queries](#saved-queries).
- **Typed result cells** (`Null / Bool / Int / UInt / Float / Decimal /
  Text / Bytes / Timestamp / Date / Time / Uuid / Other`) — preserved end-to-end so the
  CSV / TSV / JSON / SQL exporters keep type fidelity. The TUI renders each
  cell via its own `display()`; `NULL` cells are dimmed. The bottom row of
  the expanded result view shows the full value of the selected cell so
  wide values stay readable when the column is narrower than them.
- **Yank and export** in the expanded result view: `y` copies the
  current cell to the clipboard, `v` enters Visual mode for a
  rectangular selection and `y` then copies it as CSV/TSV/JSON/SQL, and
  `:export csv|tsv|json|sql` exports the selection (or, without one, the
  whole latest result).
  `:export sql` infers the source table from simple `SELECT … FROM t`
  shapes; pass it explicitly for joins/aggregates. Add a path to write
  to disk instead of the clipboard.
- **Column tools** in the expanded view: reorder (`H`/`L`), hide (`x`),
  and reset (`R`) columns.
- **SQL autocomplete** — syntax-aware popover via sqlparser's tokenizer.
  Completes keywords, tables, columns, SQL functions, and CTE names with
  FROM/JOIN alias resolution. Auto-triggers in Insert mode after `.` or
  2+ identifier chars (`Ctrl+Space` forces it). Fuzzy-ranked, kind-boosted,
  dialect-quoted on insert. Schema cache primed at connect time; columns
  load lazily; DDL run from rowdy auto-reloads. See the
  [Autocomplete](#autocomplete) reference for the full behaviour.
- **SQL formatter** — `=` formats the statement under the cursor (or the
  Visual selection); `:format` does the same and `:format all` formats
  the whole buffer.
- **Vim search & replace** — `/` search highlights every match (`Esc`
  clears them), and `:[range]s/pattern/replacement/[gic]` substitutes,
  with an interactive confirm for the `c` flag — see
  [Search & replace](#search--replace).
- **Command-bar completion** — typing after `:` pops up matching command
  names; `Tab` accepts.
- **Which-key hints** — pressing `<Space>` or `Ctrl+W` pops up the keys
  that can follow (leader overrides from `keybindings.toml` included).
- **Mouse support** — click to focus panels and toggle schema nodes,
  scroll the schema / chat / results with the wheel, click an inline
  result cell to open the expanded view at that cell, and drag to
  select cells in the expanded view.
- **Three SQL drivers** sharing the same `Datasource` trait:
  - **SQLite** — in-memory or file-based, schema via `sqlite_master` and
    `pragma_*` virtual tables.
  - **Postgres** — schema via `pg_namespace` + `information_schema`, indices
    via `pg_class`/`pg_index` for the uniqueness flag. User-defined `ENUM`
    types decode to their variant string.
  - **MySQL / MariaDB** — schema via `information_schema`, `column_type` for
    declared types (preserves `unsigned`, display widths, etc.).
- **LLM chat companion** in the right panel — multi-provider (OpenAI,
  Anthropic, Ollama, Google, DeepSeek, OpenRouter), streaming, with
  first-class tools for the schema, the editor buffer, and (read-only)
  the project files:
  - **Schema tools** (`list_catalogs` / `list_schemas` / `list_tables` /
    `describe_table`) auto-load on first use against the live connection,
    so the model can answer "describe `users`" without you expanding the
    panel by hand.
  - **Buffer tools** — `read_buffer` (paginated by line) lets the model
    ground answers in the actual SQL you have; `write_buffer` is a
    precise find/replace that errors when the search snippet matches
    zero or multiple times, so the model can't accidentally clobber the
    buffer.
  - **File tools** — `read_file`, `list_directory`, `grep_files`, locked
    to the directory rowdy was started in and refusing `.env*` files.
    By default each call asks for approval
    (`read_tools = "off" | "ask" | "auto"`, set in `:chat settings`).
  - **`AGENTS.md`** at the project root is loaded into the system
    prompt; ones in subdirectories are picked up as the file tools
    touch them.
  - **No execute-SQL tool** by design — drafts land in the editor and you
    review/run them yourself.
  - **Modal chat panel** — Normal mode (scrolling, globals work) and
    Insert mode (composer focused). Press `i` to type, `Esc` to scroll;
    `Esc` again returns focus to the editor without flipping the panel.
  - **Per-connection session persistence** — chat history saves to
    `./.rowdy/chats/<connection>/session.jsonl` and reloads on the next
    connect. `:chat clear` wipes it.
  - **Encrypted API keys** — when the store is password-protected, the
    LLM keystore shares the same crypto block as saved connections, so
    one password unlocks both (with a plaintext store, keys are stored
    in plaintext too).
- **14 bundled themes** with a live-preview picker (`:theme`) — see
  [Themes](#themes). Theme + schema-panel width persist to
  `./.rowdy/config.toml`.
- **Saved connections** in `./.rowdy/config.toml`, optionally encrypted with a
  password (argon2id + chacha20-poly1305). Pick one from `:conn`, switch live
  with `:conn use NAME`. The password prompts in-TUI on launch or via
  `--password`. Manage them without the TUI via `rowdy connections …`. The
  connection form includes `Ctrl+T` to test a URL before saving.
- **Per-connection editor sessions** persisted at
  `./.rowdy/sessions/<name>/session_<N>.sql` — several buffers per
  connection (`:session new`, `<Space>n`, `<Space>0`–`9`). The buffer is
  flushed 800ms after the last edit and reloaded on the next launch (or
  `:conn use` switch).
- **Vim-style modal input** end-to-end: editor uses real vim bindings via
  edtui; the schema panel and result viewer use the same `hjkl` / `gg` / `G`
  vocabulary.
- **File logger** at `./.rowdy/<datetime>.log` — connect / execute / cancel /
  errors. URL passwords are redacted; only the 5 most recent logs are kept.

## Keybindings

rowdy uses **vim-style bindings everywhere**. Three layers determine what a
key does: the active screen or overlay (prompt, popup, expanded result), the focused panel, and the editor's own vim
mode (Normal / Insert / Visual / Search).

### Global

Available wherever the editor is in vim Normal or Visual mode, or the
schema/chat panel is focused in its own Normal mode — i.e. **everywhere
except a text-input mode** (editor Insert, chat composer, modal forms).
In an insert mode, press `Esc` first.

| Keys                  | Action                                                  |
|-----------------------|---------------------------------------------------------|
| `:`                   | Open command prompt                                     |
| `Esc` (Schema/Chat)   | Focus editor (right panel keeps painting whatever it had) |
| `Ctrl+W` then `h`/`l` | Focus editor / right panel (schema or chat)             |
| `Ctrl+W` then `<`/`>` | Grow / shrink schema panel width                        |
| `<` / `>`             | Grow / shrink schema panel width (from any panel)       |
| `=`                   | Format SQL (editor Visual: selection; otherwise the statement under the cursor) |
| `Q`                   | Hide the inline result preview (same as `:close`)       |
| `Ctrl+C`              | Quit (same as `:q`) — from anywhere except the `:` prompt, password prompt and connection form, where it copies |

### Clipboard

In the text inputs (auth prompt, connection form, `:` command prompt,
LLM settings, query-parameter prompt, chat composer) the standard
system-clipboard shortcuts are wired up. In the last three, plain
`Ctrl+C` quits rowdy — use `Ctrl+Shift+C` / `Cmd+C` to copy there.

| Keys                                                    | Action |
|---------------------------------------------------------|--------|
| `Ctrl+V` / `Ctrl+Shift+V` / `Cmd+V`                     | Paste  |
| `Ctrl+C` / `Ctrl+Shift+C` / `Cmd+C` (with selection)    | Copy   |
| `Ctrl+X` / `Ctrl+Shift+X` / `Cmd+X` (with selection)    | Cut    |

`Ctrl+U` clears the field in all of them.

Copy and cut are suppressed in the password prompt — exposing the masked buffer
would defeat the masking. Bracketed paste from the terminal (which is what
most macOS terminals deliver for `Cmd+V`) is also accepted.

In the SQL editor, edtui's vim bindings drive the clipboard: `y` yanks,
`p` pastes, `d` cuts. They go through the system clipboard automatically
(via `arboard`), so you can yank in rowdy and paste into another app, or
vice versa.

### Leader chord (Space)

App-wide — fires from editor Normal/Visual, the schema panel, or chat
normal mode. Not in any insert mode (Esc out first).

| Keys        | Action                                                           |
|-------------|------------------------------------------------------------------|
| `<Space> r` | (Editor Normal) Highlight the statement under the cursor, prompt to run |
| `<Space> r` | (Editor Visual) Run the current selection — no prompt            |
| `<Space> R` | Run the statement under the cursor immediately — no prompt       |
| `<Space> e` | Expand the latest result to full view                            |
| `<Space> c` | Cancel the in-flight query                                       |
| `<Space> S` | Switch right panel to schema (and focus it)                      |
| `<Space> C` | Switch right panel to chat (and focus it, in normal mode)        |
| `<Space> n` | Cycle to the next per-connection editor session                  |
| `<Space> 0`…`<Space> 9` | Switch directly to session 0…9                       |

`Ctrl+Space` opens the SQL autocomplete popover (editor focus, any vim
mode).

After `<Space>` (or `Ctrl+W`) a hint popover lists every key that can
follow; it closes as soon as you press the next key.

![The <Space> leader hint popover](docs/screenshots/leader-hints.png)

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

The same prompt (with `⚠ DELETE without WHERE — confirm to run` and
similar) guards destructive statements on every run path.

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

### Chat panel

Toggle the right panel to chat with `:chat` or `<Space> C`. The panel is
**modal**, like edtui: it opens in **Normal mode** (composer dormant,
keystrokes scroll the log) and you press `i` to enter **Insert mode**
(composer focused, type your message). Globals (`:`, leader, `Ctrl+W`)
fire from Normal but not from Insert — `Esc` drops you back.

#### Normal mode (composer dormant)

| Keys              | Action                                                     |
|-------------------|------------------------------------------------------------|
| `i` / `I`         | Enter Insert mode (focus the composer)                     |
| `↑` / `k` / `h`   | Scroll the message log up by one line                      |
| `↓` / `j` / `l`   | Scroll the message log down by one line                    |
| `PgUp` / `PgDn`   | Scroll by a page                                           |
| `gg` / `G`        | Jump to the top / bottom of the log (G re-engages auto-follow) |
| `Home` / `End`    | Jump to the top / bottom of the log                        |
| `Esc`             | Focus editor (right panel keeps painting chat)             |

#### Insert mode (composer focused)

| Keys              | Action                                                     |
|-------------------|------------------------------------------------------------|
| `Enter`           | Submit the composer · `Shift+Enter` inserts a newline      |
| `Esc`             | Drop back to chat Normal (composer keeps its contents)     |
| `Ctrl+U`          | Clear the composer (message log untouched)                 |
| `Ctrl+W` then `h` | Hop directly to the editor without leaving Insert first    |
| `PgUp` / `PgDn`   | Scroll the message log by a page                           |
| `Ctrl+↑` / `Ctrl+↓` | Scroll the message log line by line (plain `↑`/`↓` move the composer cursor) |
| `Ctrl+Home` / `Ctrl+End` | Jump to top / bottom of the log                     |

The log **auto-follows** new content while you're at the bottom. Scrolling
up disengages auto-follow so streaming tokens don't yank you away from
history; scrolling back to the bottom (or `G` / `Ctrl+End`) re-engages it.

#### Tools the model uses

The chat panel registers nine tools the model can call. Schema and
buffer tools run against the live app state (a schema cache miss asks
the DB worker to introspect); file tools run on a blocking thread.

| Tool             | Purpose                                                      |
|------------------|--------------------------------------------------------------|
| `list_catalogs`  | List databases on the active connection                      |
| `list_schemas`   | List schemas inside a catalog                                |
| `list_tables`    | List tables / views inside a (catalog, schema)               |
| `describe_table` | Columns + types for a table or view; auto-loads on first use |
| `read_buffer`    | Read the editor buffer with line pagination (`start_line`, `limit`) |
| `write_buffer`   | Find/replace in the editor buffer — `search` must match exactly once (`start_line` constrains scope); zero or multiple matches return an error so the model extends the snippet |
| `read_file`      | Read a project file (paginated)                              |
| `list_directory` | List a project directory                                     |
| `grep_files`     | Regex search across project files                            |

The three file tools are jailed to the project root (the directory
rowdy was started in) and refuse any `.env` / `.env.*` path. With the
default `read_tools = "ask"` each call pauses on
`🔒 chat wants to read … y to allow · n/Esc to deny`; `auto` runs them
without asking and `off` doesn't register them at all.

Schema tools auto-trigger introspection on cache miss, so the model can
ask about a table you haven't expanded yet. `write_buffer` parks the
cursor at the modified line so the change is immediately visible.

#### Provider configuration

Open the settings modal with `:chat settings`. Pick a backend (OpenAI,
Anthropic, Ollama, Google, DeepSeek, OpenRouter — all compiled in),
enter a model id, an optional base URL, and an API key, and choose the
read-tools mode (Off / Ask / Auto). When the store is password-protected,
keys are encrypted with the same `argon2id + chacha20-poly1305` block
used for connection passwords, so unlocking the store at startup
unlocks both.

### Expanded result view

When you've expanded a result block (`<Space>e` or `:expand`).

| Keys         | Action                                                     |
|--------------|------------------------------------------------------------|
| `h j k l` / arrows | Move cell cursor                                     |
| `0` / `$` (`Home` / `End`) | First / last column in row                   |
| `gg` / `G`   | First / last row                                           |
| `y`          | Yank — current cell (Normal) or selection (Visual, prompts for format) |
| `v`          | Toggle Visual mode (rectangular cell selection)            |
| `H` / `L`    | Move the current column left / right                       |
| `x`          | Hide the current column                                    |
| `R`          | Reset column order and visibility                          |
| `:`          | Command bar — `:export` acts on the Visual selection        |
| `q` / `Esc`  | Visual: exit Visual · Normal: close expanded view          |

The mouse works here too: the wheel scrolls and dragging selects a
rectangle (entering Visual mode).

![Expanded result view with a Visual selection and the yank-format prompt](docs/screenshots/results.png)

When the result has more rows or columns than fit on screen the view
scrolls to keep the cursor visible. The title shows `rows X-Y of Z` and
`cols X-Y of Z` with `↑`/`↓` and `‹`/`›` markers when there is content
off-screen, plus the cursor position and, in Visual mode, the selection
size. The inline preview shows only the leftmost columns that fit and a
`+N →` count of how many were truncated — expand it to navigate.

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
With an active Visual selection in the expanded view it exports just the
rectangle (and `:export sql <table>` lets you name the target table for
it); otherwise it exports the latest result block in full.

Pass a path after the format to write to disk instead of the clipboard:
`:export csv path/to/out.csv`. A leading `>` (as its own word) is
optional and ignored (`:export csv > out.csv` is the same call). For
`sql` the first word is the table name, so write
`:export sql <table> out.sql` or `:export sql > out.sql` (inferred
table) — `:export sql out.sql` would treat `out.sql` as the table. `~` and `~/` expand to
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
    asks for `:export sql <table>`. A Visual selection only requires
    the *selected* projection items to satisfy the projection rule,
    so on a single-table query you can still yank the clean columns
    even if others are aliased or computed (joins never infer).
  - **Limitations**. No `CREATE TABLE` prelude (target schema must
    already exist), no `BEGIN`/`COMMIT` wrapping, no `ON CONFLICT` /
    `ON DUPLICATE KEY` clauses; selecting a column subset that
    excludes `NOT NULL` columns won't round-trip cleanly.

### Autocomplete

The popover **auto-opens** in editor Insert mode after you type `.` or
at least N identifier characters (default 2, configurable via
`completion_trigger_threshold` in `~/.rowdy/config.toml`; `0` means only
after `.`); `Ctrl+Space` forces it open in any editor mode.

![Column completion after an alias](docs/screenshots/autocomplete.png)

Selection and acceptance:

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
  CTE bodies with named columns surface those columns; `SELECT *`
  bodies resolve against the schema cache when the FROM has a single
  base table.
- After `SELECT` / `WHERE` / `ON` / `AND` / `OR` / `HAVING` /
  `ORDER BY` / `GROUP BY` / `SET` / `USING` / `IN` / `(` / `,` /
  `INSERT INTO` column list / comparison and arithmetic operators →
  **columns** unioned across FROM/JOIN bindings (qualifier-free) plus
  **SQL functions** (per-dialect curated list). With no FROM bindings
  yet, these slots fall back to the mixed list below.
- Statement start or after `;` → **keywords**.
- Any other slot → a **mixed** list: keywords, functions, bound
  columns, and CTE names.

FROM/JOIN/UPDATE/INSERT aliases are resolved by a forward pass over the *whole*
statement, so `SELECT u.|` autocompletes correctly even when the
`FROM users u` clause comes after the cursor. CTE definitions
(`WITH name AS (…)`, optional `RECURSIVE`, multiple comma-separated
CTEs) and derived-table aliases are recognized; their bodies'
projections are extracted for column completion.

**Ranking.** Candidates are scored with `nucleo-matcher` (fuzzy
subsequence match) and re-ordered by:

1. Score (higher = better): the fuzzy score, plus +500 when the label
   has the user's exact prefix, plus a **kind bonus** matched to the
   syntactic context — `+1000` for columns in column slots and tables
   in table slots (`+1100` for CTEs there), `+200` for columns in mixed
   slots, `+100` for keywords in keyword slots, `+50` for functions in
   mixed slots — so a shorter coincidentally-matching keyword can't
   shadow the right answer.
2. Shorter labels first.
3. Alphabetical.

With nothing typed yet (e.g. right after `.`) there's no fuzzy step:
kind bonus first, then alphabetical.

**Insert.** Accepting an item writes into the buffer with three
refinements:

- **Quoting** when the identifier can't sit unquoted: any uppercase
  char (Postgres folds unquoted to lowercase), any non-`[A-Za-z0-9_]`
  char, leading digit, or one of the curated reserved keywords. The
  quote style follows the dialect — `"x"` for SQLite/Postgres,
  `` `x` `` for MySQL — and any internal quote chars are doubled.
  Keywords and functions are inserted *as displayed*, never quoted.
- **Trail** depends on item kind:
  - **Table / view / CTE** → a trailing space, then a one-item popover
    offering a short alias (e.g. `u` for `users`, from the first letters
    of underscore segments) — `Enter`/`Tab` accepts it, `Esc` skips it.
  - **Function** with arguments → appended `()` with the cursor
    between them, ready for arguments.
  - **Function** with no arguments (`NOW`, `CURDATE`, …) → appended
    `()`, cursor at the end. SQL-standard niladic forms
    (`CURRENT_DATE`, `CURRENT_TIME`, `CURRENT_TIMESTAMP`) are inserted
    bare.
  - **Column / keyword** → no trail.

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
| `Enter`          | Submit (completes the highlighted command first if the popover is open) |
| `Esc`            | Cancel                  |
| `Tab`            | Accept the highlighted command completion |
| `Up` / `Down` / `Ctrl+P` / `Ctrl+N` | Move the completion selection |
| `Ctrl+U`         | Clear the line          |
| `Backspace`      | Delete character        |
| `Left` / `Right` | Move cursor             |
| typing           | Insert character (command names are prefix-matched in a popover) |

Pressing `:` with an editor Visual selection pre-fills `'<,'>` for
[substitute](#search--replace).

## Commands

| Command                      | Effect                                                          |
|------------------------------|-----------------------------------------------------------------|
| `:q`, `:quit`                | Quit                                                            |
| `:help`, `:?`                | Open the help popover (bindings + commands)                     |
| `:run`, `:r`                 | Run the statement under the cursor (no confirmation)            |
| `:cancel`                    | Cancel the in-flight query                                      |
| `:expand`, `:e`              | Expand the latest result                                        |
| `:collapse`, `:c`            | Close the expanded result view                                  |
| `:close`, `:hide`            | Hide the inline result preview (the next query shows it again; also `Q`) |
| `:width <cols>`              | Set schema panel width (clamped 12–80)                          |
| `:theme`                     | Open the theme picker                                           |
| `:theme <name>`              | Switch to a bundled theme (e.g. `:theme nord`)                  |
| `:export csv` \| `tsv` \| `json` `[path]` | Copy the latest result (or the expanded view's Visual selection) to the clipboard, or write to `path` if given |
| `:export sql [table] [path]` | Emit `INSERT` statements. Table is inferred from the query for simple `SELECT * FROM t` / `SELECT cols FROM t` shapes; pass `<table>` explicitly for joins, aggregates, aliases, etc. `:export sql > path` writes to disk with inferred table |
| `:format`, `:fmt`            | Format the statement under the cursor (or active Visual selection) via `sqlformat`. Undo via edtui's `u` won't restore the pre-format text — yank first if you need a backup |
| `:format all`                | Format the whole buffer                                         |
| `:[range]s/pat/repl/[gic]`   | Vim-style substitute — see [Search & replace](#search--replace) |
| `:save <name>`               | Save the Visual selection (or statement under the cursor) as a named query for this connection; asks before overwriting |
| `:load <name>`               | Insert a saved query at the cursor                              |
| `:run-saved [name]`          | Run a saved query; with no name, open a picker                  |
| `:update`                    | Check GitHub for a newer release now                            |
| `:reload`                    | Drop and re-prime the autocomplete schema cache against the active connection (use after DDL outside the app) |
| `:reset`                     | Roll back any open transaction on the pinned session connection and drop it; the next query re-acquires a fresh connection |
| `:clear`                     | Wipe the editor buffer, drop result blocks, and reset the session (useful after a multi-statement script you'd like to walk away from cleanly) |
| `:source`                    | Re-read user + project UI prefs, the user keybindings file, and LLM provider records. Connections, crypto, the worker pool, and any in-flight query are NOT touched |
| `:conn`, `:conn list`        | Open the connection list (or the create form if none are saved) |
| `:conn add [name]`           | Open the form to create a connection                            |
| `:conn edit <name>`          | Open the form pre-filled with `<name>`'s URL (overwrite on save) |
| `:conn delete <name>`        | Remove `<name>` (refuses if it's the active connection)         |
| `:conn use <name>`           | Switch the active connection live                               |
| `:chat`                      | Toggle the right panel between schema and chat (focus follows) |
| `:chat clear`                | Wipe the chat log and the persisted session for this connection |
| `:chat settings`, `:chat config` | Open the LLM provider modal (backend / model / API key)     |
| `:session`, `:session list`  | Show the connection's session indices and the active one (bottom bar) |
| `:session next` \| `prev`    | Cycle through the connection's sessions (`<Space>n` does next)  |
| `:session new`               | Create a fresh session at the lowest unused index and switch to it |
| `:session <N>`               | Switch to session `<N>` (must already exist)                    |
| `:session delete <N>`        | Delete session `<N>` (refuses if it's the only remaining one)   |

Aliases: `:conns`, `:conn ls` / `rm`, `:sess`, `:session ls` /
`previous` / `rm`.

### Query parameters

Statements containing `$1`-style or `:name` placeholders (outside string
literals and comments; `?` is deliberately not a placeholder) open a
prompt before they run. Values are spliced into the SQL **as text**, so
quote strings yourself (`'shipped'`); `NULL` or any SQL expression also
works. The last values you used are remembered per connection.

| Keys                | Action                 |
|---------------------|------------------------|
| `Tab` / `Shift+Tab` | Next / previous field  |
| `Enter`             | Run                    |
| `Esc`               | Cancel                 |
| `Ctrl+U`            | Clear the field        |

![The query-parameter prompt](docs/screenshots/query-params.png)

### Saved queries

`:save <name>` stores the Visual selection (or the statement under the
cursor) as `./.rowdy/queries/<connection>/<name>.sql` — plain files you
can edit or grep outside rowdy. `:load <name>` inserts one at the
cursor; `:run-saved <name>` runs it through the normal pipeline
(parameter prompt, destructive guard, multi-statement), and bare
`:run-saved` opens a picker (`j`/`k`, `Enter` to run, `Esc` to close).

### Search & replace

`/pattern` searches the editor buffer and highlights every match;
`Esc` in Normal mode clears the highlights.

`:[range]s/pattern/replacement/[flags]` substitutes, vim-style:

- **Range** — none (cursor line), `%` (whole buffer), `N,M`, `.`, `$`,
  or `'<,'>` (the Visual selection; pressing `:` in Visual pre-fills it).
- **Flags** — `g` every match on the line, `i` case-insensitive, `c`
  confirm each match (`y` replace · `n` skip · `a` all · `l` last ·
  `q`/`Esc` stop).
- **Regex** — Rust's [`regex`](https://docs.rs/regex) syntax, not vim's.
  In the replacement, `$1` / `${name}` / `\1` reference groups, `&` or
  `\0` the whole match; write `$$` for a literal `$` and `\&` for a
  literal `&`.

![Interactive substitute (:%s/…/…/gc)](docs/screenshots/substitute.png)

### Themes

14 themes are compiled in: `base16-dark` (default), `dark`, `dracula`,
`gruber-darker`, `gruvbox`, `monokai`, `neon`, `nord`, `rose-pine`,
`solarized-dark`, `base16-light`, `github-light`, `light`,
`solarized-light`. Bare `:theme` opens a picker with a live preview
(`j`/`k` or arrows, `g`/`G`, `Enter` to apply, `Esc` to cancel);
`:theme <name>` switches directly. The editor's syntax highlighting
follows the theme.

![The theme picker previewing gruvbox](docs/screenshots/theme-picker.png)

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

The codebase is a small, MVC-flavoured loop with async workers on the side:

```
            ┌──────────────────────────────────────────────────┐
            │  tokio runtime                                   │
            │                                                  │
            │  main task (event loop)                          │
            │  ┌───────────────────────────────────────────┐   │
            │  │ select!:                                  │   │
            │  │   crossterm EventStream  → Action         │   │
            │  │   worker → app channel   → Action::Worker │   │
            │  │   session-save debounce · elapsed tick    │   │
            │  └───────────────────────────────────────────┘   │
            │              │                  ▲                │
            │      cmd_tx  │                  │  evt_rx        │
            │              ▼                  │                │
            │  worker task          (evt_rx is also fed by the │
            │                        LLM chat worker and the   │
            │                        update check)             │
            │  ┌───────────────────────────────────────────┐   │
            │  │ owns Arc<dyn Datasource> (sqlx::Pool)     │   │
            │  │ tracks current query JoinHandle           │   │
            │  │ Connect / Execute / Cancel / Introspect / │   │
            │  │ ResetSession / completion-cache priming   │   │
            │  └───────────────────────────────────────────┘   │
            └──────────────────────────────────────────────────┘
```

- `App` (`src/app.rs`) owns the entire UI state and the `cmd_tx` handle.
- `Action` (`src/action/mod.rs`, with per-feature submodules) enumerates
  the legal mutations; `apply()` is the dispatcher. Parsed `:` commands
  (`src/command.rs`) go through `dispatch_command`, which calls some
  helpers directly.
- `event::translate` (`src/event.rs`) is a pure function that turns a
  `crossterm::Event` (key, paste, or mouse) into an `Action` based on the
  current overlay, screen, focus, pending chord, and popovers. Rebindable
  keys are looked up in the `Keymap` (`src/keybindings/`).
- View functions under `src/ui/` derive from `App`; the only thing they
  write back is render-time bookkeeping (the mouse hit-test layout cache
  and clamped scroll offsets).
- Each database is a `Driver` (`src/datasource/driver.rs`): a stateless
  descriptor carrying everything dialect-specific — URL schemes, the
  sqlparser dialect, identifier quoting, autocomplete functions, and
  SQL-literal rendering for `:export sql` — plus `connect()`, which
  returns a live `Datasource` (`src/datasource/mod.rs`):
  `introspect_catalogs`, `introspect_schemas`, `introspect_tables`,
  `introspect_columns`, `introspect_indices`, `default_schema`,
  `execute`, `cancel`, `reset_session`. Drivers live under
  `src/datasource/sql/` and are registered in `DRIVERS`; adding a
  database touches nothing outside `src/datasource/`.
- The worker (`src/worker/mod.rs`) owns the live connection pool, runs at
  most one query at a time, and fans introspection out concurrently.
  `:cancel` sends a server-side cancel (`pg_cancel_backend` for Postgres,
  `KILL QUERY` for MySQL) *and* aborts the in-flight `JoinHandle`, so the
  database doesn't keep grinding on a query the user gave up on. SQLite
  has no server-side cancel; the abort is the cancel. Stoolap gets a
  cancellation flag, but the engine only checks it at a few points (not
  inside joins), so rowdy also moves the session to a fresh executor: the
  next statement doesn't wait behind the runaway one, and any open
  transaction is rolled back.

State is encoded so that invalid combinations are unrepresentable wherever
possible:

- `Focus { Editor, Schema, Chat, ChatComposer }` — exactly one panel owns
  input; `PendingChord { None, Window, Leader, GG }` tracks a half-typed
  chord.
- `Screen { Normal, ResultExpanded { id, cursor, col_offset, row_offset,
  view, column_view }, Auth(AuthState), EditConnection(ConnFormState),
  ConnectionList(ConnListState), ThemePicker(ThemePickerState) }` — the
  full-screen state, always present.
- `Option<Overlay>` floats on top: `Command(CommandBuffer)`,
  `ConfirmRun { statement, reason }`, `Connecting { name }`, `Help`,
  `LlmSettings`, `UpdateAvailable`, `ParamsPrompt`, `ConfirmToolUse`,
  `ConfirmSaveOverwrite`, `SavedQueryPicker`, `ConfirmSubstitute`. Every
  variant carries the data its UI needs; no "expanded but no result", no
  "in command mode but no buffer", no "awaiting confirmation but no
  statement".
- `QueryStatus { Idle, Running, Succeeded, Failed, Cancelled, Notice }` —
  replaces a bag of booleans / `Option<String>` fields.
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
  command.rs              `:` command parser
  event.rs                crossterm Event → Action translation (keys, paste, mouse)
  cli.rs                  clap arg parsing (-c/--connection NAME, -p/--password)
  clipboard.rs            arboard wrapper for paste/copy/cut into inputs
  crypto.rs               argon2id KDF + chacha20poly1305 AEAD primitives
  connections.rs          ConnectionStore: encrypt/decrypt, unlock, make_entry
  config.rs               ./.rowdy/config.toml load + lazy save
  user_config.rs          ~/.rowdy/config.toml (user defaults, update state)
  log.rs                  Logger — Arc<Mutex<File>>, info/warn/error
  export.rs               CSV / TSV / JSON / SQL formatters for yank + :export
  sql_infer.rs            source-table inference for :export sql
  sql_quote.rs            dialect-aware identifier quoting
  session.rs              .rowdy/sessions/<name>/session_<N>.sql load + save
  chat_session.rs         .rowdy/chats/<name>/session.jsonl persistence
  saved_queries.rs        .rowdy/queries/<name>/*.sql (:save / :load / :run-saved)
  param_history.rs        .rowdy/params/<name>.json (last-used parameter values)
  substitute.rs           :s/pattern/replacement/flags engine
  update.rs               GitHub release check + self-update
  subcommands.rs          non-TUI `rowdy connections …` handlers
  terminal.rs             terminal init / restore / panic hook
  action/                 Action enum + apply() (mod.rs) and per-feature handlers:
                          auth, chat, completion, conn_form, conn_list,
                          llm_settings, params_prompt, query, results,
                          saved_queries, schema, session, substitute, update
  autocomplete/           SQL completion: context detection, ranking engine,
                          schema cache, keywords/functions, insert rules, DDL reload
  keybindings/            keybindings.toml: chord parser, action IDs, keymap
  llm/                    chat: provider mapping, keystore, prompt, tools
                          (+ tools/schemas.rs), fs_root jail, AGENTS.md, worker
  state/                  sub-state modules: editor, schema, results, command,
                          focus, screen, overlay, status, layout, auth,
                          conn_form, conn_list, chat, completion, llm_settings,
                          params_prompt, right_panel, saved_query_picker,
                          substitute_confirm, theme_picker
  datasource/
    mod.rs                Datasource trait, DRIVERS registry, connect()
    driver.rs             Driver trait (per-database dialect + connect)
    literal.rs            shared SQL-literal rendering for :export sql
    cell.rs               typed Cell enum + display helpers
    schema.rs             CatalogInfo / SchemaInfo / TableInfo / …
    error.rs              DatasourceError
    sql/
      mod.rs              shared helpers (statement splitting, destructive guard, …)
      placeholders.rs     $N / :name placeholder scanning + splicing
      sqlite.rs           SqliteDriver + SqliteDatasource (sqlx)
      postgres.rs         PostgresDriver + PostgresDatasource (sqlx)
      mysql.rs            MysqlDriver + MysqlDatasource (sqlx, also mariadb://)
      stoolap.rs          StoolapDriver + StoolapDatasource (embedded, in-process)
  worker/
    mod.rs                tokio worker task, command/event channels
    request.rs            RequestId newtype + counter
  ui/
    mod.rs                render() — layout + cursor placement
    theme.rs              TOML theme registry (themes/*.toml, compiled in)
    editor_view.rs, schema_view.rs, results_view.rs, chat_view.rs,
    bottom_bar.rs, help_view.rs, leader_hints_view.rs, theme_picker_view.rs,
    autocomplete_popover.rs, command_completion_popover.rs, auth_view.rs,
    conn_form_view.rs, conn_list_view.rs, llm_settings_view.rs,
    params_prompt_view.rs, saved_query_picker_view.rs
themes/                   the 14 bundled themes (embedded at build time)
examples/
  seed_sqlite.rs          creates a sample SQLite DB to test against
  probe_sqlite.rs         small SQLite type-decoding probe
docs/screenshots/         README screenshots + the vhs tapes that generate them
```

## Roadmap

Next likely steps, roughly ordered:

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
