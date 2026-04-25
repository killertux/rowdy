use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "rowdy", about = "Terminal SQL client", version)]
pub struct Args {
    /// Connection string. Scheme dispatches to the driver
    /// (e.g. `mock://`, `sqlite://path/to.db`, `postgres://user@host/db`).
    #[arg(long)]
    pub connection: String,
}
