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

use crate::ui::theme::ThemeKind;

pub const FILE_NAME: &str = "config.toml";
pub const DIR_NAME: &str = ".rowdy";

/// `$HOME/.rowdy/` (or `%USERPROFILE%\.rowdy\`). `None` when neither
/// env var is set — rare, but possible in stripped CI containers.
pub fn user_data_dir() -> Option<PathBuf> {
    #[allow(deprecated)] // `home_dir` un-deprecated in 1.86; rust-version pins ≥ 1.86.
    std::env::home_dir().map(|h| h.join(DIR_NAME))
}

/// Resolve effective theme using project-overrides-user precedence.
/// Defaulted to `ThemeKind::Dark` when neither store pins a value.
pub fn effective_theme(project: Option<ThemeKind>, user: Option<ThemeKind>) -> ThemeKind {
    project.or(user).unwrap_or(ThemeKind::Dark)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_width: Option<u16>,
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

    /// Lazy mkdir on first write. No runtime callers yet; reserved for
    /// a future `:set` command that writes user-side prefs.
    #[allow(dead_code)]
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
            theme: Some(ThemeKind::Light),
            schema_width: Some(48),
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: UserConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed, cfg);
        // Default `Option::None` fields are skipped on serialise.
        assert!(!text.contains("None"));
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
        assert_eq!(
            effective_theme(Some(ThemeKind::Light), Some(ThemeKind::Dark)),
            ThemeKind::Light
        );
        // Only user pinned ⇒ user wins.
        assert_eq!(
            effective_theme(None, Some(ThemeKind::Light)),
            ThemeKind::Light
        );
        // Only project pinned ⇒ project wins.
        assert_eq!(
            effective_theme(Some(ThemeKind::Light), None),
            ThemeKind::Light
        );
        // Neither pinned ⇒ compiled default.
        assert_eq!(effective_theme(None, None), ThemeKind::Dark);
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
            theme: Some(ThemeKind::Light),
            schema_width: None,
        };
        let project_theme: Option<ThemeKind> = None;
        assert_eq!(
            effective_theme(project_theme, user.theme),
            ThemeKind::Light
        );
    }

    #[test]
    fn layered_project_overrides_user_theme() {
        // A.2: user dark + project light → project wins.
        let user = UserConfig {
            theme: Some(ThemeKind::Dark),
            schema_width: None,
        };
        let project_theme = Some(ThemeKind::Light);
        assert_eq!(
            effective_theme(project_theme, user.theme),
            ThemeKind::Light
        );
    }

    #[test]
    fn layered_neither_pinned_yields_default_and_no_dir_created() {
        // A.3: neither file pins theme → default Dark; loader does
        // not auto-create the user dir.
        let parent = tempdir();
        let user_dir = parent.join(".rowdy-not-created");
        assert!(!user_dir.exists());
        let user_store = UserConfigStore::load(&user_dir).unwrap();
        assert!(!user_dir.exists(), "load() must not create user dir");
        let user = user_store.state();
        let project_theme: Option<ThemeKind> = None;
        assert_eq!(effective_theme(project_theme, user.theme), ThemeKind::Dark);
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
        project_store.set_theme(ThemeKind::Dark).unwrap();

        // Project file got `theme = "dark"`.
        let project_text =
            fs::read_to_string(project_dir.join(crate::config::FILE_NAME)).unwrap();
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
