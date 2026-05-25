//! Self-update flow: version-check responses, install-script dispatch,
//! and the deferred-prompt promotion driven by the main loop.

use crate::app::App;
use crate::state::focus::Focus;
use crate::state::overlay::Overlay;
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;
use crate::worker::WorkerEvent;

pub(super) fn on_update_up_to_date(app: &mut App, current: String) {
    app.log.info(
        "update",
        format!("manual check: rowdy v{current} is the latest"),
    );
    app.status = QueryStatus::Notice {
        msg: format!("✓ rowdy v{current} is the latest"),
    };
}

pub(super) fn on_update_check_failed(app: &mut App, error: String) {
    app.log
        .warn("update", format!("manual check failed: {error}"));
    app.status = QueryStatus::Failed {
        error: format!("update check: {error}"),
    };
}

pub(super) fn on_update_available(app: &mut App, current: String, latest: String) {
    // Stash for later instead of opening the overlay immediately.
    // Showing the prompt during startup (Auth screen, ConnectionList,
    // Connecting overlay) would steal keyboard input from the password
    // prompt or silently dismiss itself if the user types `n` in their
    // password. `try_promote_pending_update` (called once per main-loop
    // tick) does the deferred handoff once the user reaches Normal.
    app.log.info(
        "update",
        format!("update {latest} pending; will prompt when user is idle"),
    );
    app.pending_update_prompt = Some((current, latest));
}

/// Move a queued update prompt from `App::pending_update_prompt` onto
/// the live `Overlay` once the user is on `Screen::Normal` with no
/// active overlay and is not actively typing. Idempotent and cheap —
/// safe to call from the run loop on every iteration.
pub fn try_promote_pending_update(app: &mut App) {
    if app.pending_update_prompt.is_none() {
        return;
    }
    if !matches!(app.screen, Screen::Normal) {
        return;
    }
    if app.overlay.is_some() {
        return;
    }
    // Don't capture keystrokes from a user actively typing.
    if matches!(app.focus, Focus::ChatComposer) {
        return;
    }
    if matches!(app.focus, Focus::Editor)
        && !matches!(
            app.editor.editor_mode(),
            edtui::EditorMode::Normal | edtui::EditorMode::Visual
        )
    {
        return;
    }
    let Some((current, latest)) = app.pending_update_prompt.take() else {
        return;
    };
    app.overlay = Some(Overlay::UpdateAvailable { current, latest });
}

pub(super) fn on_update_installed(app: &mut App, tag: String) {
    app.log
        .info("update", format!("install.sh succeeded for {tag}"));
    app.status = QueryStatus::Notice {
        msg: format!("✓ updated to {tag} — restart rowdy to use it"),
    };
}

pub(super) fn on_update_install_failed(app: &mut App, error: String) {
    app.log
        .warn("update", format!("install.sh failed: {error}"));
    app.status = QueryStatus::Failed {
        error: format!("update failed: {error}"),
    };
}

pub(super) fn apply_update_accept(app: &mut App) {
    let Some(Overlay::UpdateAvailable { latest, .. }) = app.overlay.take() else {
        return;
    };
    app.status = QueryStatus::Notice {
        msg: format!("⬇ downloading {latest}…"),
    };
    let install_dir = match std::env::current_exe() {
        Ok(exe) => exe.parent().map(std::path::Path::to_path_buf),
        Err(err) => {
            app.log.warn("update", format!("current_exe failed: {err}"));
            None
        }
    };
    let Some(install_dir) = install_dir else {
        app.status = QueryStatus::Failed {
            error: "update failed: cannot resolve install dir".into(),
        };
        return;
    };
    let evt_tx = app.evt_tx.clone();
    let logger = app.log.clone();
    let tag = latest.clone();
    tokio::spawn(async move {
        let event = match crate::update::run_installer(&tag, &install_dir).await {
            Ok(()) => WorkerEvent::UpdateInstalled { tag },
            Err(error) => {
                logger.warn("update", format!("installer error: {error}"));
                WorkerEvent::UpdateInstallFailed { error }
            }
        };
        let _ = evt_tx.send(event);
    });
}

pub(super) fn apply_check_for_update(app: &mut App) {
    app.status = QueryStatus::Notice {
        msg: "checking for updates…".into(),
    };
    // Drop any stale auto-check that hasn't been promoted yet — the
    // manual check is authoritative and will re-stash if a newer
    // release is still available.
    app.pending_update_prompt = None;
    crate::update::spawn_manual_check(app.evt_tx.clone(), env!("CARGO_PKG_VERSION").to_string());
}

pub(super) fn apply_update_dismiss(app: &mut App) {
    let Some(Overlay::UpdateAvailable { latest, .. }) = app.overlay.take() else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Err(err) = app.user_config.record_check(now, Some(latest.clone())) {
        app.log
            .warn("update", format!("persisting dismissal failed: {err}"));
    } else {
        app.log.info("update", format!("user dismissed {latest}"));
    }
}
