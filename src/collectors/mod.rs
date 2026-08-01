//! Data collectors: one per source, each returning a snapshot struct.
//!
//! - [`vllm`] — scrape vLLM `/metrics` (Prometheus exposition text)
//! - [`sglang`] — scrape SGLang `/metrics` (Prometheus exposition text)
//! - [`access_log`] — tail a log file or `docker logs` for the request feed
//!   (vLLM request-log lines from `--enable-log-requests`)
//! - [`common`] — shared exposition-text parser + HTTP fetcher
//!
//! [`make_collector`] builds the right collector from a [`BackendKind`]; with
//! [`BackendKind::Auto`] an [`AutoCollector`] probes once and delegates to the
//! detected backend.

pub mod access_log;
pub mod common;
pub mod sglang;
pub mod vllm;

use std::sync::Mutex;
use std::time::Duration;

use crate::state::BackendSnapshot;

/// A metrics backend collector: produces a [`BackendSnapshot`] per poll.
pub trait Backend: Send + Sync {
    fn poll(&self) -> BackendSnapshot;
}

/// Which backend to scrape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendKind {
    /// Probe `/metrics` once and pick vllm or sglang from the metric prefix.
    #[default]
    Auto,
    Vllm,
    Sgl,
}

impl BackendKind {
    /// Parse a case-insensitive backend name. Accepts `sgl` and `sglang`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "vllm" => Some(Self::Vllm),
            "sgl" | "sglang" => Some(Self::Sgl),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vllm => "vllm",
            Self::Sgl => "sglang",
        }
    }
}

/// Build a backend collector for the given kind.
pub fn make_collector(kind: BackendKind, metrics_url: String, timeout: f64) -> Box<dyn Backend> {
    match kind {
        BackendKind::Vllm => Box::new(vllm::VllmCollector::new(metrics_url, timeout)),
        BackendKind::Sgl => Box::new(sglang::SglCollector::new(metrics_url, timeout)),
        BackendKind::Auto => Box::new(AutoCollector::new(metrics_url, timeout)),
    }
}

/// Auto-detecting collector: probes `/metrics` once, picks vllm or sglang from
/// the metric-name prefix, then delegates all subsequent polls to the chosen
/// backend. The first poll's body is parsed immediately (no double fetch).
pub struct AutoCollector {
    url: String,
    timeout: Duration,
    inner: Mutex<Option<Box<dyn Backend>>>,
}

impl AutoCollector {
    pub fn new(url: String, timeout: f64) -> Self {
        Self {
            url,
            timeout: Duration::from_secs_f64(timeout.max(0.001)),
            inner: Mutex::new(None),
        }
    }

    /// Sniff the backend from the exposition text by looking for the first
    /// `vllm:` or `sglang:` metric name. Falls back to vllm.
    fn detect(text: &str) -> BackendKind {
        for line in text.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if t.starts_with("vllm:") {
                return BackendKind::Vllm;
            }
            if t.starts_with("sglang:") {
                return BackendKind::Sgl;
            }
            // Keep scanning — python/process lines come before backend metrics.
        }
        BackendKind::Vllm
    }
}

impl Backend for AutoCollector {
    fn poll(&self) -> BackendSnapshot {
        let mut guard = self.inner.lock().unwrap();
        if let Some(ref c) = *guard {
            return c.poll();
        }
        // First poll: fetch once, sniff, parse with the detected backend, and
        // store the real collector for subsequent polls.
        match common::fetch_metrics_text(&self.url, self.timeout) {
            Ok(text) => {
                let kind = Self::detect(&text);
                let parsed = match kind {
                    BackendKind::Vllm => vllm::parse_metrics(&text),
                    BackendKind::Sgl => sglang::parse_metrics(&text),
                    BackendKind::Auto => vllm::parse_metrics(&text),
                };
                *guard = Some(make_collector(
                    kind,
                    self.url.clone(),
                    self.timeout.as_secs_f64(),
                ));
                parsed
            }
            Err(e) => BackendSnapshot {
                reachable: false,
                error: Some(e),
                ..BackendSnapshot::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_parse() {
        assert_eq!(BackendKind::parse("auto"), Some(BackendKind::Auto));
        assert_eq!(BackendKind::parse("VLLM"), Some(BackendKind::Vllm));
        assert_eq!(BackendKind::parse("sglang"), Some(BackendKind::Sgl));
        assert_eq!(BackendKind::parse("SGL"), Some(BackendKind::Sgl));
        assert_eq!(BackendKind::parse("nope"), None);
    }

    #[test]
    fn detect_vllm() {
        assert_eq!(
            AutoCollector::detect("python_info{...} 1\nvllm:num_requests_running 3"),
            BackendKind::Vllm
        );
    }

    #[test]
    fn detect_sglang() {
        assert_eq!(
            AutoCollector::detect("# HELP ...\nsglang:num_running_reqs 162"),
            BackendKind::Sgl
        );
    }

    #[test]
    fn detect_defaults_to_vllm() {
        assert_eq!(
            AutoCollector::detect("python_info{...} 1\n"),
            BackendKind::Vllm
        );
    }
}
