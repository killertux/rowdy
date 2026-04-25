use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rowdy", about = "Terminal SQL client", version)]
pub struct Args {
    /// Subcommand. Omit to launch the TUI.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Name of a saved connection to open (managed via `:conn add` / `:conn
    /// edit` in the TUI). Optional on first launch when there are no
    /// connections yet. Ignored when a subcommand is given.
    #[arg(long, short = 'c', global = true)]
    pub connection: Option<String>,

    /// Password for the encrypted connection store. If omitted and the store
    /// is encrypted, the TUI prompts for it. `--password ""` is treated the
    /// same as omitting it.
    ///
    /// Note: visible in `ps` and shell history. Prefer the in-TUI prompt for
    /// production use.
    #[arg(long, short = 'p', global = true)]
    pub password: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage saved connections without launching the TUI.
    #[command(subcommand)]
    Connections(ConnCommand),
}

#[derive(Debug, Subcommand)]
pub enum ConnCommand {
    /// List all saved connection names.
    #[command(alias = "ls")]
    List,
    /// Add a new connection.
    Add {
        /// Connection name (used as the lookup key for `--connection`).
        name: String,
        /// Connection URL (e.g. `sqlite:./sample.db`,
        /// `postgres://user@host/db`, `mysql://...`, `mariadb://...`).
        #[arg(long)]
        url: String,
    },
    /// Overwrite an existing connection's URL.
    Edit {
        name: String,
        #[arg(long)]
        url: String,
    },
    /// Remove a saved connection.
    #[command(alias = "rm")]
    Delete { name: String },
}
