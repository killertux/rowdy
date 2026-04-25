use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::cli::ConnCommand;
use crate::config::ConfigStore;
use crate::connections::{self, ConnectionStore};

/// Entry point for all `rowdy connections …` subcommands. Returns the exit
/// code (`0` on success).
pub fn run_connections(data_dir: &Path, cmd: ConnCommand, password: Option<String>) -> Result<i32> {
    let mut config = ConfigStore::load(data_dir)
        .with_context(|| format!("loading config from {}", data_dir.display()))?;
    match cmd {
        ConnCommand::List => list(&config),
        ConnCommand::Add { name, url } => upsert(&mut config, password, name, url, "added"),
        ConnCommand::Edit { name, url } => upsert(&mut config, password, name, url, "overwrote"),
        ConnCommand::Delete { name } => delete(&mut config, name),
    }
}

fn list(config: &ConfigStore) -> Result<i32> {
    let mode = if config.is_encrypted() {
        "encrypted"
    } else {
        "plaintext"
    };
    let names = config.connection_names();
    if names.is_empty() {
        println!("no connections ({mode} store)");
        return Ok(0);
    }
    for name in &names {
        println!("{name}");
    }
    println!();
    let count = names.len();
    println!(
        "{count} connection{plural} ({mode} store)",
        plural = if count == 1 { "" } else { "s" },
    );
    Ok(0)
}

fn upsert(
    config: &mut ConfigStore,
    supplied_password: Option<String>,
    name: String,
    url: String,
    verb: &str,
) -> Result<i32> {
    let store = open_store(config, supplied_password)?;
    let entry = store
        .make_entry(name.clone(), &url)
        .map_err(|e| anyhow!("encrypt failed: {e}"))?;
    config
        .upsert_connection(entry)
        .with_context(|| format!("save connection {name:?}"))?;
    let mode = if store.is_encrypted() {
        "encrypted"
    } else {
        "plaintext"
    };
    println!("{verb} {name:?} ({mode})");
    Ok(0)
}

fn delete(config: &mut ConfigStore, name: String) -> Result<i32> {
    let removed = config
        .delete_connection(&name)
        .with_context(|| format!("delete connection {name:?}"))?;
    if removed {
        println!("deleted {name:?}");
        Ok(0)
    } else {
        eprintln!("rowdy: no connection named {name:?}");
        Ok(1)
    }
}

/// Resolves the connection store for write operations. May prompt for a
/// password (interactive) or initialise crypto if this is the first encrypted
/// entry. We treat `--password` carefully:
/// - flag absent → prompt only when we have to (encrypted store, or a fresh
///   store the user hasn't decided on yet)
/// - `--password ""` → explicit "no encryption"; never prompt
/// - `--password X` → use X
fn open_store(config: &mut ConfigStore, supplied: Option<String>) -> Result<ConnectionStore> {
    let supplied: PasswordChoice = match supplied {
        None => PasswordChoice::NoFlag,
        Some(s) if s.is_empty() => PasswordChoice::Plaintext,
        Some(s) => PasswordChoice::Provided(s),
    };

    match (config.crypto().cloned(), supplied) {
        (Some(block), PasswordChoice::Provided(pw)) => unlock_inner(&block, &pw),
        (Some(_), PasswordChoice::Plaintext) => Err(anyhow!(
            "store is encrypted; pass --password <pw> (empty would lock you out)"
        )),
        (Some(block), PasswordChoice::NoFlag) => {
            let pw = rpassword::prompt_password("Password: ").context("read password")?;
            unlock_inner(&block, &pw)
        }
        (None, PasswordChoice::Provided(pw)) => initialise(config, &pw),
        (None, PasswordChoice::Plaintext) => Ok(ConnectionStore::plaintext()),
        (None, PasswordChoice::NoFlag) if config.connections().is_empty() => {
            let pw = rpassword::prompt_password("Password (empty = no encryption): ")
                .context("read password")?;
            if pw.is_empty() {
                Ok(ConnectionStore::plaintext())
            } else {
                initialise(config, &pw)
            }
        }
        (None, PasswordChoice::NoFlag) => Ok(ConnectionStore::plaintext()),
    }
}

enum PasswordChoice {
    /// `--password` not given on the CLI.
    NoFlag,
    /// `--password ""` — explicit "no encryption" / plaintext mode.
    Plaintext,
    /// `--password X` with a non-empty value.
    Provided(String),
}

fn unlock_inner(block: &crate::config::CryptoBlock, pw: &str) -> Result<ConnectionStore> {
    let key = connections::unlock(pw, block).map_err(|e| anyhow!("unlock failed: {e}"))?;
    Ok(ConnectionStore::encrypted(key))
}

fn initialise(config: &mut ConfigStore, pw: &str) -> Result<ConnectionStore> {
    let (block, key) =
        connections::initialise_crypto(pw).map_err(|e| anyhow!("crypto setup failed: {e}"))?;
    config.set_crypto(block).context("save crypto block")?;
    Ok(ConnectionStore::encrypted(key))
}
