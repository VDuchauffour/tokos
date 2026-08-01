//! Structured logging initialization for tokos.
//!
//! In TUI mode, logs MUST NOT go to stdout (ratatui owns it as the alt-screen)
//! or stderr (most terminals render stderr on the same surface, corrupting the
//! TUI). Use `--trace-file` to direct logs to a file in TUI mode.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use tracing_subscriber::EnvFilter;

/// Output format for structured logs.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum LogFormat {
    /// Human-readable text (one line per event).
    Text,
    /// Structured JSON (one JSON object per event).
    Json,
}

impl LogFormat {
    fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

/// Initialize the global tracing subscriber.
///
/// - `level`: fallback filter level if `RUST_LOG` is unset (e.g. "warn", "info", "off").
/// - `file`: if `Some(path)`, write logs to that file (appended); if `None`, write to stderr.
/// - `ansi`: whether to use ANSI color codes in output.
/// - `format`: output format (text or JSON).
///
/// Respects `RUST_LOG` env var first (via `EnvFilter::try_from_default_env`),
/// falling back to the provided `level`.
///
/// Exactly one sink is installed (file on success, stderr on file-open failure
/// or when `file` is `None`), so `init()` is invoked exactly once.
pub fn init(level: &str, file: Option<&str>, ansi: bool, format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    match file {
        Some(path) => match OpenOptions::new()
            .create(true)
            .append(true)
            .open(Path::new(path))
        {
            Ok(f) => init_with_writer(filter, ansi, format, f),
            Err(e) => {
                eprintln!("tokos: cannot open log file {path}: {e}; falling back to stderr");
                init_with_writer(filter, ansi, format, io::stderr);
            }
        },
        None => {
            init_with_writer(filter, ansi, format, io::stderr);
        }
    }
}

/// Build and install the subscriber against the given writer, branching on
/// `format` to select the `.json()` or default text formatter. The HRTB bound
/// matches `with_writer`'s requirement and is satisfied by both `File` and
/// `Stderr`; `Send + Sync` ensures the installed subscriber is thread-safe.
fn init_with_writer<W>(filter: EnvFilter, ansi: bool, format: LogFormat, writer: W)
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    if format.is_json() {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .with_ansi(ansi)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer)
            .with_ansi(ansi)
            .init();
    }
}
