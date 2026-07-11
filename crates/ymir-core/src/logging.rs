//! Process logging: a backend for the `log` facade that writes to stderr and, optionally, a file.
//!
//! Diagnostics (a project loaded with degradations, an evaluation problem) go through the `log`
//! macros so they surface wherever the process runs: a GUI's captured stderr, or a headless CLI
//! run's stderr and logfile — the latter is what a toolchain reads when something quietly
//! degrades. Each binary installs the backend once at startup with [`init`]; call sites just use
//! `log::warn!` and friends and stay ignorant of where the output goes.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use log::{LevelFilter, Metadata, Record};

/// A logger that writes each record to stderr and, when configured, appends it to a file.
struct TeeLogger {
    /// The logfile, if one was opened; `None` means stderr-only.
    file: Option<Mutex<std::fs::File>>,
    level: LevelFilter,
}

impl log::Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            humantime::format_rfc3339_seconds(SystemTime::now()),
            record.level(),
            record.target(),
            record.args(),
        );
        // Logging must never fail the application, so write errors (a closed pipe, a full disk)
        // are deliberately dropped.
        let _ = std::io::stderr().write_all(line.as_bytes()); // shortcut-ok: a broken stderr must not crash the app
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.write_all(line.as_bytes()); // shortcut-ok: an unwritable logfile must not crash the app
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush(); // shortcut-ok: flush is best-effort
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let _ = file.flush(); // shortcut-ok: flush is best-effort
        }
    }
}

/// Installs the process logger, writing at or below `level` to stderr and, when `log_path` is
/// given, appending to that file (creating it and its parent). Falls back to stderr-only if the
/// file cannot be opened. Call once at startup; a later call is a no-op (the first logger wins),
/// so a second binary entry or a test does not error.
pub fn init(log_path: Option<&Path>, level: LevelFilter) {
    let file = log_path.and_then(|path| {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent); // shortcut-ok: fall back to stderr-only if the dir is unavailable
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });
    let logger = TeeLogger { file, level };
    // `set_boxed_logger` errors only if a logger is already installed; ignore so a second `init`
    // is a harmless no-op rather than a startup failure.
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
}
