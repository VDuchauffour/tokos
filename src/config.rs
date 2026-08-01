//! Runtime configuration: defaults, app config, and env-variable support.
//!
//! [`AppConfig`] is the single source of truth passed to every collector and
//! the UI. Defaults live as constants so they can be imported by tests and the
//! `--dump-json` path.

use std::env;

use crate::collectors::BackendKind;

pub const DEFAULT_URL: &str = "http://localhost:8000";
pub const DEFAULT_INTERVAL: f64 = 1.0;

/// How many samples to keep per series. Generous so charts have history to
/// resample from; resampled down to panel width at draw time.
pub const HISTORY_LEN: usize = 512;

/// Network timeout for a single /metrics GET (seconds). Kept short so a stalled
/// server can't wedge the background poller.
pub const HTTP_TIMEOUT: f64 = 2.0;

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub url: String,
    pub interval: f64,
    pub history_len: usize,
    pub http_timeout: f64,
    /// Which inference backend to scrape (`vllm`, `sglang`, or `auto`).
    pub backend: BackendKind,
    /// Activity panel: tail a log file or stream a container's `docker logs`.
    pub log_file: Option<String>,
    pub docker_container: Option<String>,
}

impl AppConfig {
    pub fn metrics_url(&self) -> String {
        let base = self.url.trim_end_matches('/');
        format!("{base}/metrics")
    }

    pub fn has_log_source(&self) -> bool {
        self.log_file.is_some() || self.docker_container.is_some()
    }

    pub fn from_env() -> Self {
        Self {
            url: env::var("TOKOS_URL").unwrap_or_else(|_| DEFAULT_URL.to_string()),
            interval: DEFAULT_INTERVAL,
            history_len: HISTORY_LEN,
            http_timeout: HTTP_TIMEOUT,
            backend: env::var("TOKOS_BACKEND")
                .ok()
                .and_then(|s| BackendKind::parse(&s))
                .unwrap_or_default(),
            log_file: env::var("TOKOS_LOG_FILE").ok(),
            docker_container: env::var("TOKOS_DOCKER").ok(),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_URL.to_string(),
            interval: DEFAULT_INTERVAL,
            history_len: HISTORY_LEN,
            http_timeout: HTTP_TIMEOUT,
            backend: BackendKind::default(),
            log_file: None,
            docker_container: None,
        }
    }
}
