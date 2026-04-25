use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
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
