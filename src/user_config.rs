//! User-level config at `$HOME/.rowdy/config.toml` (cross-platform).
//!
//! Resolved via [`std::env::home_dir`] so the same logic works on Unix
//! (`$HOME`) and Windows (`%USERPROFILE%`). The directory is **not**
//! auto-created — [`UserConfigStore::load`] only reads, and writes are
//! lazy via [`UserConfigStore::flush`].
//!
//! Project-level [`crate::config::ConfigStore`] overrides this on a
//! per-field basis; see `main.rs::run_app`.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How the chat agent's filesystem read tools (`read_file`,
/// `list_directory`, `grep_files`) are gated.
///
/// - [`Off`] strips the tools from the LLM's tool list entirely; the
///   model can't see them and won't try to call them. Use this when
///   you don't want the agent reading project files at all.
/// - [`Ask`] (the default) registers the tools but pauses each call
///   on a y/n approval overlay before executing. Trades a few
///   keystrokes for predictability.
/// - [`Auto`] registers the tools and runs them without prompting. Use
///   when you trust the model to read what it needs.
///
/// Persisted lower-case in `~/.rowdy/config.toml` so future variants
/// don't break round-trips.
///
/// [`Off`]: ReadToolsMode::Off
/// [`Ask`]: ReadToolsMode::Ask
/// [`Auto`]: ReadToolsMode::Auto
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadToolsMode {
    Off,
    #[default]
    Ask,
    Auto,
}

impl ReadToolsMode {
    /// Cycle through the three states. `delta = 1` advances forward
    /// (Off → Ask → Auto → Off), `delta = -1` reverses. Wraps in both
    /// directions.
    pub fn cycled(self, delta: i32) -> Self {
        let order = [Self::Off, Self::Ask, Self::Auto];
        let idx = order.iter().position(|m| *m == self).unwrap_or(1) as i32;
        let n = order.len() as i32;
        order[((idx + delta).rem_euclid(n)) as usize]
    }

    /// Short label used in the settings row and the system-prompt
    /// active-context block. Stable across builds — UI style changes
    /// build on top.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }
}

pub const FILE_NAME: &str = "config.toml";
pub const DIR_NAME: &str = ".rowdy";

/// `$HOME/.rowdy/` (or `%USERPROFILE%\.rowdy\`). `None` when neither
/// env var is set — rare, but possible in stripped CI containers.
pub fn user_data_dir() -> Option<PathBuf> {
    #[allow(deprecated)] // `home_dir` un-deprecated in 1.86; rust-version pins ≥ 1.86.
    std::env::home_dir().map(|h| h.join(DIR_NAME))
}

/// Default theme name used when neither store pins one. Matches a file
/// in `themes/` (`themes/base16-dark.toml`).
pub const DEFAULT_THEME_NAME: &str = "base16-dark";

/// Resolve effective theme name using project-overrides-user precedence.
/// Returns `DEFAULT_THEME_NAME` when neither store pins a value. The
/// caller is responsible for resolving the name into a [`Theme`] via
/// `Theme::by_name`, falling back to the default theme on miss.
pub fn effective_theme(project: Option<&str>, user: Option<&str>) -> String {
    project.or(user).unwrap_or(DEFAULT_THEME_NAME).to_string()
}

/// Same precedence for the schema panel width.
pub fn effective_schema_width(project: Option<u16>, user: Option<u16>, default: u16) -> u16 {
    project.or(user).unwrap_or(default)
}

/// On-disk shape of `$HOME/.rowdy/config.toml`.
///
/// Every field is `Option<T>` so "unset" stays distinguishable from "set
/// to a value that happens to equal the default" — the merge in
/// `main.rs::run_app` only consults user fields when the project file
/// did not pin them.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserConfig {
    /// Theme file stem (e.g. `"dark"`, `"light"`, or any user-shipped
    /// `themes/<name>.toml`). Unknown names fall back softly to the
    /// compiled default at App seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_width: Option<u16>,
    /// Master switch for the startup auto-update check. `None` is
    /// treated as enabled by `update::should_check`; flip to
    /// `Some(false)` in `~/.rowdy/config.toml` to opt out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_for_updates: Option<bool>,
    /// Unix seconds of the last successful release-API call. Drives
    /// the 24h throttle so we don't hit GitHub on every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update_check_at: Option<i64>,
    /// Tag the user said "no" to most recently. We suppress the
    /// prompt while the latest release equals this value so we don't
    /// pester them; a *newer* release lifts the suppression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dismissed_version: Option<String>,
    /// Three-state gate for the chat agent's filesystem read tools.
    /// `None` resolves to [`ReadToolsMode::Ask`] (the default — see
    /// [`ReadToolsMode`]). Toggleable from the `:chat settings` modal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_tools: Option<ReadToolsMode>,
}

/// Owns the on-disk user config. Vanilla runs never touch the
/// filesystem under `$HOME` — load() returns defaults on NotFound and
/// flush() is the only path that creates the directory.
#[derive(Debug)]
pub struct UserConfigStore {
    path: PathBuf,
    state: UserConfig,
}

impl UserConfigStore {
    /// Loads from `<dir>/config.toml`. Missing file ⇒ defaults. Missing
    /// directory ⇒ defaults (no error). Malformed TOML ⇒ Err with the
    /// path baked into the message, mirroring the project loader at
    /// `src/config.rs::ConfigStore::load`.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let path = dir.join(FILE_NAME);
        let state = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid user config at {}: {e}", path.display()),
                )
            })?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => UserConfig::default(),
            Err(err) => return Err(err),
        };
        Ok(Self { path, state })
    }

    /// Empty store at `dir/config.toml` without touching disk. Used for
    /// the "no $HOME" branch and for tests.
    pub fn empty(dir: &Path) -> Self {
        Self {
            path: dir.join(FILE_NAME),
            state: UserConfig::default(),
        }
    }

    pub fn state(&self) -> &UserConfig {
        &self.state
    }

    /// Persist the auto-update bookkeeping fields. `now_unix` is the
    /// timestamp of the just-completed release-API call;
    /// `dismissed_version` is set when the user said "no" to a prompt
    /// (suppressing it on subsequent launches until a newer release
    /// lands). Calls `flush()` so the result hits disk before the
    /// next process starts.
    pub fn record_check(
        &mut self,
        now_unix: i64,
        dismissed_version: Option<String>,
    ) -> io::Result<()> {
        self.state.last_update_check_at = Some(now_unix);
        if dismissed_version.is_some() {
            self.state.last_dismissed_version = dismissed_version;
        }
        self.flush()
    }

    /// Persist the chat-side filesystem-read-tools mode. Triggered by
    /// the `:chat settings` modal. We always flush eagerly so the next
    /// launch picks up the user's preference.
    pub fn set_read_tools_mode(&mut self, mode: ReadToolsMode) -> io::Result<()> {
        self.state.read_tools = Some(mode);
        self.flush()
    }

    /// Lazy mkdir on first write.
    pub fn flush(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(&self.state)
            .map_err(|e| io::Error::other(format!("serialise user config: {e}")))?;
        fs::write(&self.path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Env-var tests are process-global. Serialise via this guard so
    /// concurrent test runs don't race on `HOME` / `USERPROFILE`.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-user-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn load_missing_returns_defaults() {
        let dir = tempdir();
        let store = UserConfigStore::load(&dir).unwrap();
        assert_eq!(store.state(), &UserConfig::default());
        assert_eq!(store.state().theme, None);
        assert_eq!(store.state().schema_width, None);
    }

    #[test]
    fn load_does_not_create_directory() {
        let parent = tempdir();
        let absent = parent.join("user-rowdy-not-created");
        assert!(!absent.exists());
        let _ = UserConfigStore::load(&absent).unwrap();
        assert!(!absent.exists(), "load() must not create the user dir");
    }

    #[test]
    fn load_malformed_returns_err() {
        let dir = tempdir();
        fs::write(dir.join(FILE_NAME), "this is = not [valid toml").unwrap();
        let err = UserConfigStore::load(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("invalid user config"), "msg was {msg}");
    }

    #[test]
    fn load_empty_file_returns_defaults() {
        let dir = tempdir();
        fs::write(dir.join(FILE_NAME), "").unwrap();
        let store = UserConfigStore::load(&dir).unwrap();
        assert_eq!(store.state(), &UserConfig::default());
    }

    #[test]
    fn round_trip_user_config_with_values() {
        let cfg = UserConfig {
            theme: Some("light".into()),
            schema_width: Some(48),
            check_for_updates: Some(false),
            last_update_check_at: Some(1_730_000_000),
            last_dismissed_version: Some("v0.7.0".into()),
            read_tools: Some(ReadToolsMode::Auto),
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: UserConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed, cfg);
        // Default `Option::None` fields are skipped on serialise.
        assert!(!text.contains("None"));
    }

    #[test]
    fn read_tools_mode_round_trips_each_variant() {
        for mode in [ReadToolsMode::Off, ReadToolsMode::Ask, ReadToolsMode::Auto] {
            let cfg = UserConfig {
                read_tools: Some(mode),
                ..Default::default()
            };
            let text = toml::to_string_pretty(&cfg).unwrap();
            let parsed: UserConfig = toml::from_str(&text).unwrap();
            assert_eq!(parsed.read_tools, Some(mode));
        }
    }

    #[test]
    fn read_tools_mode_default_is_ask() {
        // The whole point of having a default: a fresh install gets
        // a sane, conservative gate. Ask is conservative — the user
        // sees every fs read before it happens.
        assert_eq!(ReadToolsMode::default(), ReadToolsMode::Ask);
    }

    #[test]
    fn read_tools_mode_cycles_forward_and_back() {
        assert_eq!(ReadToolsMode::Off.cycled(1), ReadToolsMode::Ask);
        assert_eq!(ReadToolsMode::Ask.cycled(1), ReadToolsMode::Auto);
        assert_eq!(ReadToolsMode::Auto.cycled(1), ReadToolsMode::Off);
        assert_eq!(ReadToolsMode::Off.cycled(-1), ReadToolsMode::Auto);
        assert_eq!(ReadToolsMode::Auto.cycled(-1), ReadToolsMode::Ask);
        assert_eq!(ReadToolsMode::Ask.cycled(-1), ReadToolsMode::Off);
    }

    #[test]
    fn set_read_tools_mode_persists() {
        let dir = tempdir();
        for mode in [ReadToolsMode::Off, ReadToolsMode::Ask, ReadToolsMode::Auto] {
            let mut store = UserConfigStore::load(&dir).unwrap();
            store.set_read_tools_mode(mode).unwrap();
            let reloaded = UserConfigStore::load(&dir).unwrap();
            assert_eq!(reloaded.state().read_tools, Some(mode));
        }
    }

    #[test]
    fn record_check_persists_timestamp_and_dismissal() {
        let dir = tempdir();
        let mut store = UserConfigStore::load(&dir).unwrap();
        store
            .record_check(1_730_000_000, Some("v0.7.0".into()))
            .unwrap();

        let reloaded = UserConfigStore::load(&dir).unwrap();
        assert_eq!(reloaded.state().last_update_check_at, Some(1_730_000_000));
        assert_eq!(
            reloaded.state().last_dismissed_version.as_deref(),
            Some("v0.7.0")
        );
    }

    #[test]
    fn record_check_without_dismissal_keeps_existing_dismissal() {
        let dir = tempdir();
        let mut store = UserConfigStore::load(&dir).unwrap();
        store.record_check(1, Some("v0.6.9".into())).unwrap();
        // Throttle-only update (no new dismissal) should preserve the
        // earlier dismissed tag.
        store.record_check(2, None).unwrap();

        let reloaded = UserConfigStore::load(&dir).unwrap();
        assert_eq!(reloaded.state().last_update_check_at, Some(2));
        assert_eq!(
            reloaded.state().last_dismissed_version.as_deref(),
            Some("v0.6.9")
        );
    }

    #[test]
    fn round_trip_default_serialises_to_empty_table() {
        let cfg = UserConfig::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            text.trim().is_empty(),
            "default UserConfig should serialise to empty TOML, got: {text:?}"
        );
    }

    #[test]
    fn effective_theme_project_overrides_user() {
        // Both pinned ⇒ project wins.
        assert_eq!(effective_theme(Some("light"), Some("dark")), "light");
        // Only user pinned ⇒ user wins.
        assert_eq!(effective_theme(None, Some("light")), "light");
        // Only project pinned ⇒ project wins.
        assert_eq!(effective_theme(Some("light"), None), "light");
        // Neither pinned ⇒ compiled default.
        assert_eq!(effective_theme(None, None), DEFAULT_THEME_NAME);
    }

    #[test]
    fn effective_theme_passes_through_custom_name() {
        // Any string flows through — resolution to a real theme happens
        // at the call site, not here.
        assert_eq!(effective_theme(Some("gruberDarker"), None), "gruberDarker");
    }

    #[test]
    fn effective_schema_width_layered() {
        assert_eq!(effective_schema_width(Some(40), Some(50), 32), 40);
        assert_eq!(effective_schema_width(None, Some(50), 32), 50);
        assert_eq!(effective_schema_width(Some(40), None, 32), 40);
        assert_eq!(effective_schema_width(None, None, 32), 32);
    }

    #[test]
    fn layered_user_light_no_project_pin_yields_light() {
        // A.1: user theme=light, project does not pin theme.
        let user = UserConfig {
            theme: Some("light".into()),
            ..Default::default()
        };
        let project_theme: Option<String> = None;
        assert_eq!(
            effective_theme(project_theme.as_deref(), user.theme.as_deref()),
            "light"
        );
    }

    #[test]
    fn layered_project_overrides_user_theme() {
        // A.2: user dark + project light → project wins.
        let user = UserConfig {
            theme: Some("dark".into()),
            ..Default::default()
        };
        let project_theme = Some("light".to_string());
        assert_eq!(
            effective_theme(project_theme.as_deref(), user.theme.as_deref()),
            "light"
        );
    }

    #[test]
    fn layered_neither_pinned_yields_default_and_no_dir_created() {
        // A.3: neither file pins theme → compiled default; loader does
        // not auto-create the user dir.
        let parent = tempdir();
        let user_dir = parent.join(".rowdy-not-created");
        assert!(!user_dir.exists());
        let user_store = UserConfigStore::load(&user_dir).unwrap();
        assert!(!user_dir.exists(), "load() must not create user dir");
        let user = user_store.state();
        let project_theme: Option<String> = None;
        assert_eq!(
            effective_theme(project_theme.as_deref(), user.theme.as_deref()),
            DEFAULT_THEME_NAME
        );
    }

    #[test]
    fn project_set_theme_does_not_touch_user_file() {
        // A.4: runtime `:theme dark` writes to project; user file is
        // not created or modified.
        use crate::config::ConfigStore;
        let user_dir = tempdir();
        let project_dir = tempdir();

        // Pre-seed user file with theme=light to confirm it stays
        // unchanged (not just absent).
        fs::write(user_dir.join(FILE_NAME), "theme = \"light\"\n").unwrap();
        let before = fs::read_to_string(user_dir.join(FILE_NAME)).unwrap();

        // Project store is empty; set theme via the runtime mutator.
        let mut project_store = ConfigStore::load(&project_dir).unwrap();
        project_store.set_theme("dark").unwrap();

        // Project file got `theme = "dark"`.
        let project_text = fs::read_to_string(project_dir.join(crate::config::FILE_NAME)).unwrap();
        assert!(
            project_text.contains("theme = \"dark\""),
            "project config should record dark, got: {project_text}"
        );

        // User file is unchanged byte-for-byte.
        let after = fs::read_to_string(user_dir.join(FILE_NAME)).unwrap();
        assert_eq!(before, after, "user config must not be touched by :theme");
    }

    #[test]
    fn user_data_dir_resolves_under_home_override() {
        let _g = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let fake_home = tempdir();
        // SAFETY: tests run single-threaded by default in this crate; the
        // ENV_GUARD above prevents in-file races; no other thread is
        // reading HOME here. Pattern matches src/action/mod.rs:2066-2070.
        unsafe {
            std::env::set_var("HOME", &fake_home);
        }
        let resolved = user_data_dir().expect("HOME set, user_data_dir must resolve");
        assert_eq!(resolved, fake_home.join(DIR_NAME));
    }
}
