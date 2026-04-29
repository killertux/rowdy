//! Lazy GitHub-release auto-update check.
//!
//! Runs in a background tokio task spawned at startup. Reads the
//! [`UserConfigStore`] for throttle/dismissal state, fetches the
//! latest release tag from the GitHub API, compares it against the
//! compiled-in `CARGO_PKG_VERSION`, and emits a [`WorkerEvent`] when
//! an upgrade is available so the UI can prompt the user.
//!
//! The acceptance path re-invokes the existing `install.sh` via
//! `sh -c 'curl … | sh'` with `ROWDY_INSTALL_DIR` set to the running
//! binary's directory, so the new tarball overwrites whatever rowdy
//! the user is actually running.
//!
//! Failures (network, JSON, parse) are logged at warn-level and swallowed —
//! we never want a flaky GitHub API to block a user from using rowdy.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::Deserialize;
use tokio::process::Command;

use crate::log::Logger;
use crate::user_config::{UserConfig, UserConfigStore};
use crate::worker::WorkerEvent;

const RELEASES_URL: &str = "https://api.github.com/repos/killertux/rowdy/releases/latest";
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/killertux/rowdy/main/install.sh";
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const USER_AGENT: &str = concat!("rowdy/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
}

/// Spawn the background check. Cheap on the happy path: takes the
/// `evt_tx` clone and a few owned strings, then yields to the runtime.
pub fn spawn_check(
    evt_tx: tokio::sync::mpsc::UnboundedSender<WorkerEvent>,
    logger: Logger,
    current_version: String,
    user_config_dir: Option<PathBuf>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(err) = run_check(&evt_tx, &logger, &current_version, user_config_dir).await {
            logger.warn("update", format!("update check skipped: {err}"));
        }
    })
}

async fn run_check(
    evt_tx: &tokio::sync::mpsc::UnboundedSender<WorkerEvent>,
    logger: &Logger,
    current_version: &str,
    user_config_dir: Option<PathBuf>,
) -> Result<(), String> {
    let dir = user_config_dir.ok_or_else(|| "no $HOME — skipping".to_string())?;
    let mut store = UserConfigStore::load(&dir).map_err(|e| format!("load user config: {e}"))?;
    let now = unix_now();
    if !should_check(store.state(), now) {
        return Ok(());
    }
    let latest_tag = fetch_latest_tag().await.map_err(|e| e.to_string())?;
    // Update the throttle even if there's no new version, so we don't
    // call GitHub on every restart.
    store
        .record_check(now, None)
        .map_err(|e| format!("persist check timestamp: {e}"))?;

    let current = parse_version(current_version)
        .ok_or_else(|| format!("cannot parse compiled version {current_version:?}"))?;
    let Some(latest) = parse_version(&latest_tag) else {
        return Err(format!("cannot parse remote tag {latest_tag:?}"));
    };
    if latest <= current {
        logger.info(
            "update",
            format!("up to date (current {current}, latest {latest})"),
        );
        return Ok(());
    }
    if store.state().last_dismissed_version.as_deref() == Some(&latest_tag) {
        logger.info(
            "update",
            format!("user has dismissed {latest_tag}; not prompting"),
        );
        return Ok(());
    }
    logger.info(
        "update",
        format!("new release {latest_tag} available (current {current_version})"),
    );
    let _ = evt_tx.send(WorkerEvent::UpdateAvailable {
        current: current_version.to_string(),
        latest: latest_tag,
    });
    Ok(())
}

/// `true` when we should hit the GitHub API: opted in *and* either no
/// previous check or the throttle window has elapsed.
pub fn should_check(cfg: &UserConfig, now_unix: i64) -> bool {
    if cfg.check_for_updates == Some(false) {
        return false;
    }
    match cfg.last_update_check_at {
        Some(prev) => now_unix.saturating_sub(prev) >= CHECK_INTERVAL_SECS,
        None => true,
    }
}

async fn fetch_latest_tag() -> Result<String, reqwest::Error> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .build()?;
    let resp: ReleaseResponse = client
        .get(RELEASES_URL)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(resp.tag_name)
}

/// `v0.6.2` / `0.6.2` / `v1.0.0-rc.1` → [`Version`]; anything else → `None`.
pub fn parse_version(tag: &str) -> Option<Version> {
    let trimmed = tag.trim().trim_start_matches('v');
    Version::parse(trimmed).ok()
}

/// Re-run the install script for a specific tag, dropping the new
/// binary into `install_dir`. Returns `Err` with a user-presentable
/// reason on non-zero exit (typically a permissions error if rowdy is
/// installed in a system path).
pub async fn run_installer(tag: &str, install_dir: &Path) -> Result<(), String> {
    // The script is intentionally re-fetched each time rather than
    // bundled — that way we pick up any installer fixes published
    // since the running binary was built.
    let pipe =
        format!("set -eu\ncurl --proto '=https' --tlsv1.2 -fsSL {INSTALL_SCRIPT_URL} | sh\n");
    let output = Command::new("sh")
        .arg("-c")
        .arg(pipe)
        .env("ROWDY_INSTALL_DIR", install_dir)
        .env("ROWDY_VERSION", tag)
        .output()
        .await
        .map_err(|e| format!("spawn install.sh: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_line = stderr
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("install.sh failed")
        .to_string();
    Err(first_line)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_accepts_with_and_without_v_prefix() {
        assert_eq!(parse_version("v0.6.2").unwrap(), Version::new(0, 6, 2));
        assert_eq!(parse_version("0.6.2").unwrap(), Version::new(0, 6, 2));
        assert_eq!(
            parse_version(" v1.2.3 ").unwrap(),
            Version::new(1, 2, 3),
            "leading/trailing whitespace should be ignored"
        );
    }

    #[test]
    fn parse_version_accepts_pre_release() {
        let v = parse_version("v1.0.0-rc.1").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 0);
        assert_eq!(v.patch, 0);
        assert!(!v.pre.is_empty());
    }

    #[test]
    fn parse_version_rejects_garbage() {
        assert!(parse_version("").is_none());
        assert!(parse_version("garbage").is_none());
        assert!(parse_version("v0.6").is_none(), "missing patch");
        assert!(parse_version("v0").is_none());
    }

    #[test]
    fn should_check_respects_opt_out() {
        let cfg = UserConfig {
            check_for_updates: Some(false),
            ..Default::default()
        };
        assert!(!should_check(&cfg, 1_000_000_000));
    }

    #[test]
    fn should_check_runs_on_first_launch() {
        assert!(should_check(&UserConfig::default(), 1_000_000_000));
    }

    #[test]
    fn should_check_throttles_within_window() {
        let cfg = UserConfig {
            last_update_check_at: Some(1_000_000_000),
            ..Default::default()
        };
        // 23h after the last check → still throttled.
        assert!(!should_check(&cfg, 1_000_000_000 + 23 * 3_600));
        // Exactly 24h → ready.
        assert!(should_check(&cfg, 1_000_000_000 + CHECK_INTERVAL_SECS));
        // 25h → ready.
        assert!(should_check(&cfg, 1_000_000_000 + 25 * 3_600));
    }

    #[test]
    fn parse_release_response_extracts_tag_name() {
        // Trim down GitHub's actual payload to the field we care about
        // — `serde` should ignore the rest.
        let body = r#"{
            "url": "https://api.github.com/repos/killertux/rowdy/releases/123",
            "tag_name": "v0.7.0",
            "name": "Release v0.7.0",
            "draft": false
        }"#;
        let parsed: ReleaseResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.tag_name, "v0.7.0");
    }

    #[test]
    fn parse_release_response_rejects_missing_tag_name() {
        let body = r#"{ "name": "no tag" }"#;
        assert!(serde_json::from_str::<ReleaseResponse>(body).is_err());
    }
}
