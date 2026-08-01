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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::state::BackendSnapshot;

/// A metrics backend collector: produces a [`BackendSnapshot`] per poll.
pub trait Backend: Send + Sync {
    fn poll(&self) -> BackendSnapshot;

    /// The backend kind this collector is effectively using. For
    /// [`AutoCollector`] this is the *detected* kind (after the first poll);
    /// for pinned collectors it is the kind itself. The poller compares this
    /// across polls to detect a mid-session server swap and signal the UI to
    /// clear its [`History`](crate::state::History).
    fn effective_kind(&self) -> BackendKind {
        BackendKind::Auto
    }
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

/// Re-probe the backend kind every this many polls so a mid-session server
/// swap (e.g. a vLLM pod replaced by SGLang behind the same URL) is caught
/// without a restart.
const REPROBE_EVERY: u64 = 30;

/// Auto-detecting collector: probes `/metrics`, picks vllm or sglang from the
/// metric-name prefix, then delegates subsequent polls to the chosen backend.
/// The first poll's body is parsed immediately (no double fetch). Every
/// [`REPROBE_EVERY`] polls the kind is re-sniffed so a mid-session server swap
/// is caught automatically.
pub struct AutoCollector {
    url: String,
    timeout: Duration,
    inner: Mutex<Option<Box<dyn Backend>>>,
    poll_count: AtomicU64,
    current_kind: Mutex<BackendKind>,
}

impl AutoCollector {
    pub fn new(url: String, timeout: f64) -> Self {
        Self {
            url,
            timeout: Duration::from_secs_f64(timeout.max(0.001)),
            inner: Mutex::new(None),
            poll_count: AtomicU64::new(0),
            current_kind: Mutex::new(BackendKind::Auto),
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
        }
        BackendKind::Vllm
    }
}

impl Backend for AutoCollector {
    fn poll(&self) -> BackendSnapshot {
        let count = self.poll_count.fetch_add(1, Ordering::Relaxed) + 1;
        let mut guard = self.inner.lock().unwrap();

        // First poll or periodic re-probe: fetch once, sniff, and rebuild the
        // inner collector if the kind changed. The already-fetched text is
        // parsed directly so there is no double fetch on probe polls.
        let need_probe = guard.is_none() || count.is_multiple_of(REPROBE_EVERY);
        if need_probe {
            match common::fetch_metrics_text(&self.url, self.timeout) {
                Ok(text) => {
                    let kind = Self::detect(&text);
                    let prev = *self.current_kind.lock().unwrap();
                    if guard.is_none() || kind != prev {
                        *guard = Some(make_collector(
                            kind,
                            self.url.clone(),
                            self.timeout.as_secs_f64(),
                        ));
                        *self.current_kind.lock().unwrap() = kind;
                    }
                    return match kind {
                        BackendKind::Vllm => vllm::parse_metrics(&text),
                        BackendKind::Sgl => sglang::parse_metrics(&text),
                        BackendKind::Auto => vllm::parse_metrics(&text),
                    };
                }
                Err(e) => {
                    return BackendSnapshot {
                        reachable: false,
                        error: Some(e),
                        ..BackendSnapshot::default()
                    };
                }
            }
        }

        guard.as_ref().unwrap().poll()
    }

    fn effective_kind(&self) -> BackendKind {
        *self.current_kind.lock().unwrap()
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

    #[test]
    fn vllm_collector_effective_kind() {
        let c = vllm::VllmCollector::new("http://localhost:0/metrics".into(), 0.1);
        assert_eq!(c.effective_kind(), BackendKind::Vllm);
    }

    #[test]
    fn sgl_collector_effective_kind() {
        let c = sglang::SglCollector::new("http://localhost:0/metrics".into(), 0.1);
        assert_eq!(c.effective_kind(), BackendKind::Sgl);
    }

    #[test]
    fn auto_collector_initial_kind() {
        let c = AutoCollector::new("http://localhost:0/metrics".into(), 0.1);
        assert_eq!(c.effective_kind(), BackendKind::Auto);
    }
}
