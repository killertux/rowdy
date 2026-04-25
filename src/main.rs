mod action;
mod app;
mod cli;
mod datasource;
mod event;
mod state;
mod terminal;
mod ui;
mod worker;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{Event as CtEvent, EventStream};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::Stdout;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use crate::action::{Action, apply};
use crate::app::App;
use crate::cli::Args;
use crate::event::translate;
use crate::state::focus::PendingChord;
use crate::terminal::Tui;
use crate::worker::{WorkerCommand, WorkerEvent};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let args = Args::parse();
    let datasource = datasource::connect(&args.connection)
        .await
        .with_context(|| format!("connecting to {}", args.connection))?;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();

    let worker_handle = tokio::spawn(worker::run(datasource, cmd_rx, evt_tx));

    let mut tui = Tui::init()?;
    let app = App::new(cmd_tx);
    let result = run(&mut tui.terminal, app, evt_rx).await;
    Tui::restore()?;
    worker_handle.abort();

    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
    mut evt_rx: UnboundedReceiver<WorkerEvent>,
) -> Result<()> {
    let mut events = EventStream::new();
    while !app.should_quit {
        terminal.draw(|f| ui::render(&mut app, f))?;
        tokio::select! {
            terminal_event = events.next() => match terminal_event {
                Some(Ok(ev)) => process_terminal_event(&mut app, ev),
                Some(Err(err)) => return Err(err.into()),
                None => break,
            },
            worker_event = evt_rx.recv() => match worker_event {
                Some(ev) => apply(&mut app, Action::Worker(ev)),
                None => break,
            },
        }
    }
    let _ = app.cmd_tx.send(WorkerCommand::Close);
    Ok(())
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
