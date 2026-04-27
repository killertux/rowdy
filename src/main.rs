mod action;
mod app;
mod autocomplete;
mod cli;
mod clipboard;
mod command;
mod config;
mod connections;
mod crypto;
mod datasource;
mod event;
mod export;
mod log;
mod session;
mod sql_infer;
mod sql_quote;
mod state;
mod subcommands;
mod terminal;
mod ui;
mod worker;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use crossterm::event::{Event as CtEvent, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::action::{Action, apply, dispatch_connect};
use crate::app::App;
use crate::autocomplete::SchemaCache;
use crate::cli::{Args, Command};
use crate::config::ConfigStore;
use crate::connections::ConnectionStore;
use crate::event::translate;
use crate::log::Logger;
use crate::state::auth::{AuthKind, AuthState};
use crate::state::conn_form::ConnFormState;
use crate::state::conn_list::ConnListState;
use crate::state::focus::PendingChord;
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;
use crate::terminal::Tui;
use crate::worker::{WorkerCommand, WorkerEvent};

const DATA_DIR: &str = ".rowdy";
/// Cap on the number of `.rowdy/<datetime>.log` files kept on disk. Older
/// runs are deleted at startup once the new session's log has been opened.
const MAX_LOG_FILES: usize = 5;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
    match run_app().await {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            eprintln!("rowdy: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_app() -> Result<i32> {
    let args = Args::parse();
    let data_dir = init_data_dir().context("preparing .rowdy/ directory")?;

    // Subcommands run without the TUI or a log file — they're one-shot CLI
    // operations and shouldn't litter `.rowdy/` with empty session logs.
    if let Some(cmd) = args.command {
        return match cmd {
            Command::Connections(sub) => {
                subcommands::run_connections(&data_dir, sub, args.password)
            }
        };
    }

    let log_path = log_file_path(&data_dir);
    let logger = Logger::open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    logger.info(
        "rowdy",
        format!("starting; log file: {}", log_path.display()),
    );

    // Cap the on-disk log count. The just-opened log counts toward the
    // limit, so old runs only start dropping when we've actually reached
    // MAX_LOG_FILES sessions.
    if let Err(err) = log::prune_old(&data_dir, MAX_LOG_FILES, &logger) {
        logger.warn("rowdy", format!("log pruning failed: {err}"));
    }

    let mut config = ConfigStore::load(&data_dir)
        .with_context(|| format!("loading config from {}", data_dir.display()))?;

    // Resolve as much of the auth/connection picture as we can before
    // reaching for the TUI. `Decision::*` tells us how to seed the App.
    let decision = decide_startup(&mut config, &args, &logger)?;
    drop(args);

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let schema_cache = Arc::new(RwLock::new(SchemaCache::new()));
    let worker_handle = tokio::spawn(worker::run(
        logger.clone(),
        cmd_rx,
        evt_tx,
        schema_cache.clone(),
    ));

    let mut tui = Tui::init()?;
    let mut app = App::new(
        cmd_tx,
        config,
        logger.clone(),
        data_dir.clone(),
        schema_cache,
    );
    apply_decision(&mut app, decision);

    let result = run(&mut tui.terminal, &mut app, evt_rx).await;
    Tui::restore()?;
    worker_handle.abort();

    logger.info("rowdy", "shutdown");
    result.map(|()| app.exit_code)
}

/// What `decide_startup` resolved before the TUI came up.
// `AuthState` carries a TextArea (~700 bytes); the other variants are smaller.
// We construct one Decision and immediately destructure it into App, so the
// short-lived size imbalance isn't worth boxing.
#[allow(clippy::large_enum_variant)]
enum Decision {
    /// Need a password from the user.
    Auth(AuthState),
    /// Already have a store; let the user create their first connection.
    CreateFirst { store: ConnectionStore },
    /// Already have a store and a target connection — fire Connect on launch.
    AutoConnect {
        store: ConnectionStore,
        name: String,
        url: String,
    },
    /// Already have a store and saved connections, but no `--connection`
    /// hint — open the picker so the user can choose.
    PickConnection {
        store: ConnectionStore,
        names: Vec<String>,
    },
}

fn decide_startup(config: &mut ConfigStore, args: &Args, logger: &Logger) -> Result<Decision> {
    let cli_password = args.password.as_deref().filter(|p| !p.is_empty());

    // Phase 1: do we already have a store unlocked?
    let store = match (config.crypto().cloned(), cli_password) {
        (Some(block), Some(pw)) => {
            // CLI password short-circuits the in-TUI prompt.
            let key = connections::unlock(pw, &block).map_err(|e| anyhow!("unlock failed: {e}"))?;
            logger.info("auth", "unlocked via --password");
            Some(ConnectionStore::encrypted(key))
        }
        (Some(block), None) => {
            // Defer to in-TUI prompt.
            return Ok(Decision::Auth(AuthState::new(AuthKind::Unlock { block })));
        }
        (None, Some(pw)) => {
            // First setup with a CLI-supplied password — initialise crypto now.
            let (block, key) = connections::initialise_crypto(pw)
                .map_err(|e| anyhow!("crypto setup failed: {e}"))?;
            config.set_crypto(block).context("save crypto block")?;
            logger.info("auth", "encrypted store initialised via --password");
            Some(ConnectionStore::encrypted(key))
        }
        (None, None) if config.connections().is_empty() => {
            // Empty store, no CLI password — let the user choose plaintext-vs-encrypted in the prompt.
            return Ok(Decision::Auth(AuthState::new(AuthKind::FirstSetup)));
        }
        (None, None) => {
            // Existing plaintext store; skip the prompt entirely.
            Some(ConnectionStore::plaintext())
        }
    };
    let store = store.expect("store resolved");

    // Phase 2: which connection to open?
    let names = config.connection_names();
    if names.is_empty() {
        return Ok(Decision::CreateFirst { store });
    }
    if let Some(requested) = args.connection.as_deref() {
        let entry = config
            .connection(requested)
            .ok_or_else(|| anyhow!("no connection named {requested:?} (have: {names:?})"))?;
        let url = store
            .lookup(entry)
            .map_err(|e| anyhow!("decrypt {requested:?} failed: {e}"))?;
        return Ok(Decision::AutoConnect {
            store,
            name: requested.to_string(),
            url: url.to_string(),
        });
    }
    Ok(Decision::PickConnection { store, names })
}

fn apply_decision(app: &mut App, decision: Decision) {
    match decision {
        Decision::Auth(state) => {
            app.screen = Screen::Auth(state);
        }
        Decision::CreateFirst { store } => {
            app.connection_store = Some(store);
            app.screen = Screen::EditConnection(ConnFormState::new_create());
        }
        Decision::AutoConnect { store, name, url } => {
            app.connection_store = Some(store);
            dispatch_connect(app, name, url);
        }
        Decision::PickConnection { store, names } => {
            app.connection_store = Some(store);
            app.screen = Screen::ConnectionList(ConnListState::new(names));
        }
    }
}

fn init_data_dir() -> std::io::Result<PathBuf> {
    let dir = std::env::current_dir()?.join(DATA_DIR);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn log_file_path(dir: &Path) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    dir.join(format!("{stamp}.log"))
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    mut evt_rx: UnboundedReceiver<WorkerEvent>,
) -> Result<()> {
    let mut events = EventStream::new();
    while !app.should_quit {
        terminal.draw(|f| ui::render(app, f))?;
        // Read the deadlines once per iteration so the sleeps are rebuilt
        // each loop with whatever the latest edit pushed them to.
        let save_at = app.pending_save_at;
        let tick_at = elapsed_tick_deadline(app);
        tokio::select! {
            terminal_event = events.next() => match terminal_event {
                Some(Ok(ev)) => process_terminal_event(app, ev),
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            worker_event = evt_rx.recv() => match worker_event {
                Some(ev) => apply(app, Action::Worker(ev)),
                None => break,
            },
            _ = wait_until_or_pending(save_at) => {
                action::flush_session(app);
            }
            _ = wait_until_or_pending(tick_at) => {
                // Wake the loop so the elapsed counter in the bottom bar
                // advances. The redraw at the top of the next iteration
                // does the actual work.
            }
        }
    }
    if app.editor_dirty {
        action::flush_session(app);
    }
    let _ = app.cmd_tx.send(WorkerCommand::Close);
    Ok(())
}

/// Future that resolves at `at` if `Some`, or stays pending forever if `None`
/// — used so the save branch in the select! block is dormant when no save
/// is scheduled.
async fn wait_until_or_pending(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending::<()>().await,
    }
}

/// While a query is running, redraw every 500ms so the bottom-bar elapsed
/// counter ticks even if the user is idle. Returns `None` otherwise so the
/// branch stays dormant.
fn elapsed_tick_deadline(app: &App) -> Option<tokio::time::Instant> {
    matches!(app.status, QueryStatus::Running { .. })
        .then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(500))
}

fn process_terminal_event(app: &mut App, ev: CtEvent) {
    let chord_was_pending = app.pending != PendingChord::None;
    if let Some(action) = translate(app, ev) {
        apply(app, action);
    }
    if chord_was_pending {
        app.pending = PendingChord::None;
    }
}
