use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Append-only log sink shared across the app and all datasources. Cheap to
/// clone — internally an `Arc<Mutex<File>>`.
#[derive(Clone)]
pub struct Logger {
    inner: Arc<Mutex<File>>,
}

impl Logger {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(file)),
        })
    }

    /// Logger that drops every message — for tests where we don't want to
    /// touch the filesystem.
    #[cfg(test)]
    pub fn discard() -> Self {
        Self::open(Path::new("/dev/null")).expect("open /dev/null")
    }

    pub fn info(&self, target: &str, msg: impl AsRef<str>) {
        self.write(Level::Info, target, msg.as_ref());
    }

    pub fn warn(&self, target: &str, msg: impl AsRef<str>) {
        self.write(Level::Warn, target, msg.as_ref());
    }

    pub fn error(&self, target: &str, msg: impl AsRef<str>) {
        self.write(Level::Error, target, msg.as_ref());
    }

    fn write(&self, level: Level, target: &str, msg: &str) {
        let line = format!(
            "{ts} [{lvl:5}] {target}: {msg}\n",
            ts = Utc::now().to_rfc3339(),
            lvl = level.label(),
            target = target,
            msg = msg,
        );
        if let Ok(mut file) = self.inner.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// Keep at most `keep` `*.log` files in `dir`; delete the oldest until the
/// count fits. Filenames are timestamped (`YYYY-MM-DD_HH-MM-SS.log`), so a
/// lexicographic sort is chronological — oldest first. Run after the
/// current session's log has been opened so the new file is included in
/// the count and protected from deletion.
pub fn prune_old(dir: &Path, keep: usize, logger: &Logger) -> std::io::Result<()> {
    let mut logs: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "log"))
        .collect();
    if logs.len() <= keep {
        return Ok(());
    }
    logs.sort();
    let to_delete = logs.len() - keep;
    for path in &logs[..to_delete] {
        match fs::remove_file(path) {
            Ok(()) => logger.info("log", format!("pruned old log: {}", path.display())),
            Err(e) => logger.warn("log", format!("could not prune {}: {e}", path.display())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-log-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), "").unwrap();
    }

    fn log_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".log"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn prune_old_is_a_no_op_when_under_cap() {
        let dir = tempdir();
        touch(&dir, "2026-01-01_00-00-00.log");
        touch(&dir, "2026-01-02_00-00-00.log");
        prune_old(&dir, 5, &Logger::discard()).unwrap();
        assert_eq!(log_names(&dir).len(), 2);
    }

    #[test]
    fn prune_old_keeps_the_newest_n_log_files() {
        let dir = tempdir();
        // Lexicographic order is the same as chronological for this format.
        for stamp in [
            "2026-01-01_00-00-00",
            "2026-02-01_00-00-00",
            "2026-03-01_00-00-00",
            "2026-04-01_00-00-00",
            "2026-05-01_00-00-00",
            "2026-06-01_00-00-00",
            "2026-07-01_00-00-00",
        ] {
            touch(&dir, &format!("{stamp}.log"));
        }
        // Drop a non-log file that should never be touched.
        touch(&dir, "config.toml");

        prune_old(&dir, 3, &Logger::discard()).unwrap();
        assert_eq!(
            log_names(&dir),
            vec![
                "2026-05-01_00-00-00.log".to_string(),
                "2026-06-01_00-00-00.log".to_string(),
                "2026-07-01_00-00-00.log".to_string(),
            ]
        );
        // The non-log sibling survives.
        assert!(dir.join("config.toml").exists());
    }
}
