//! Collect a [`BackendSnapshot`] from an SGLang `/metrics` endpoint.
//!
//! SGLang exposes a subset of vLLM's metrics under a `sglang:` prefix.
//! [`parse_metrics`] maps them onto the shared [`BackendSnapshot`]; fields
//! SGLang doesn't expose (KV-cache %, preemptions, per-request histograms,
//! cache-config info) stay at their defaults. The `sglang:cache_hit_rate`
//! gauge is stored as `prefix_cache_hits = rate, prefix_cache_queries = 1` so
//! [`BackendSnapshot::prefix_cache_hit_rate`] returns the gauge value.
//!
//! [`parse_metrics`] is a pure function (no I/O) so it can be unit-tested
//! against a fixture.

use std::collections::HashMap;
use std::time::Duration;

use crate::collectors::common::{self, le_to_float};
use crate::state::{BackendSnapshot, Histogram};

/// Scalar series we read directly by sample name (summed across model labels).
const SCALAR_NAMES: &[&str] = &[
    "sglang:prompt_tokens_total",
    "sglang:generation_tokens_total",
    "sglang:num_running_reqs",
    "sglang:num_queue_reqs",
];

/// Histogram base names -> attribute on [`BackendSnapshot`].
const HISTOGRAMS: &[(&str, &str)] = &[
    ("sglang:time_to_first_token_seconds", "ttft"),
    ("sglang:e2e_request_latency_seconds", "e2e"),
    ("sglang:time_per_output_token_seconds", "inter_token"),
];

/// Parse Prometheus exposition text into a [`BackendSnapshot`].
pub fn parse_metrics(text: &str) -> BackendSnapshot {
    let mut snap = BackendSnapshot {
        reachable: true,
        ..BackendSnapshot::default()
    };

    let mut scalars: HashMap<&str, f64> = SCALAR_NAMES.iter().map(|&n| (n, 0.0)).collect();
    let mut hists: HashMap<&str, Histogram> = HISTOGRAMS
        .iter()
        .map(|&(b, _)| (b, Histogram::new()))
        .collect();

    for line in text.lines() {
        let Some(s) = common::parse_line(line) else {
            continue;
        };
        let name = s.name;
        let value = s.value;

        if snap.model_name.is_none()
            && let Some((_, mn)) = s.labels.iter().find(|(k, _)| *k == "model_name")
            && !mn.is_empty()
        {
            snap.model_name = Some(mn.clone());
        }

        // Process start time (stdlib process collector) -> uptime.
        if name == "process_start_time_seconds" {
            snap.process_start_time = Some(value);
            continue;
        }

        // SGLang exposes cache hit rate as a gauge in [0, 1]. Store it so that
        // prefix_cache_hit_rate() == gauge value (hits = rate, queries = 1).
        if name == "sglang:cache_hit_rate" {
            snap.prefix_cache_hits_total = value;
            snap.prefix_cache_queries_total = 1.0;
            continue;
        }

        if let Some(slot) = scalars.get_mut(name) {
            *slot += value;
            continue;
        }

        // Histograms: match the first base that `name` starts with.
        for &(base, _attr) in HISTOGRAMS {
            if !name.starts_with(base) {
                continue;
            }
            if let Some(hist) = hists.get_mut(base) {
                if name == format!("{base}_sum").as_str() {
                    hist.sum += value;
                } else if name == format!("{base}_count").as_str() {
                    hist.count += value;
                } else if name == format!("{base}_bucket").as_str() {
                    let le = s
                        .labels
                        .iter()
                        .find(|(k, _)| *k == "le")
                        .map(|(_, v)| le_to_float(v))
                        .unwrap_or(f64::INFINITY);
                    hist.add(le, value);
                }
            }
            break;
        }
    }

    snap.prompt_tokens_total = *scalars.get("sglang:prompt_tokens_total").unwrap_or(&0.0);
    snap.generation_tokens_total = *scalars
        .get("sglang:generation_tokens_total")
        .unwrap_or(&0.0);
    snap.num_requests_running = *scalars.get("sglang:num_running_reqs").unwrap_or(&0.0);
    snap.num_requests_waiting = *scalars.get("sglang:num_queue_reqs").unwrap_or(&0.0);

    // Move histograms out of the map onto the snapshot by attribute.
    for &(base, attr) in HISTOGRAMS {
        if let Some(hist) = hists.remove(base) {
            match attr {
                "ttft" => snap.ttft = hist,
                "e2e" => snap.e2e = hist,
                "inter_token" => snap.inter_token = hist,
                _ => {}
            }
        }
    }

    snap
}

/// Fetches and parses `/metrics`, returning a snapshot each poll.
pub struct SglCollector {
    metrics_url: String,
    timeout: Duration,
}

impl SglCollector {
    pub fn new(metrics_url: String, timeout: f64) -> Self {
        Self {
            metrics_url,
            timeout: Duration::from_secs_f64(timeout.max(0.001)),
        }
    }

    pub fn poll(&self) -> BackendSnapshot {
        match common::fetch_metrics_text(&self.metrics_url, self.timeout) {
            Ok(text) => parse_metrics(&text),
            Err(e) => BackendSnapshot {
                reachable: false,
                error: Some(e),
                ..BackendSnapshot::default()
            },
        }
    }
}

impl crate::collectors::Backend for SglCollector {
    fn poll(&self) -> BackendSnapshot {
        SglCollector::poll(self)
    }

    fn effective_kind(&self) -> crate::collectors::BackendKind {
        crate::collectors::BackendKind::Sgl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/sglang_metrics_fixture.txt");

    fn snap() -> BackendSnapshot {
        parse_metrics(FIXTURE)
    }

    #[test]
    fn reachable_and_model() {
        let s = snap();
        assert!(s.reachable);
        assert_eq!(
            s.model_name.as_deref(),
            Some("meta-llama/Llama-3.1-8B-Instruct")
        );
    }

    #[test]
    fn scalar_values() {
        let s = snap();
        assert_eq!(s.prompt_tokens_total, 8.128902e+06);
        assert_eq!(s.generation_tokens_total, 7.557572e+06);
        assert_eq!(s.num_requests_running, 162.0);
        assert_eq!(s.num_requests_waiting, 2826.0);
    }

    #[test]
    fn cache_hit_rate_from_gauge() {
        let s = snap();
        // The gauge value 0.007507552643049313 should round-trip through
        // prefix_cache_hit_rate() (hits = rate, queries = 1).
        assert!((s.prefix_cache_hit_rate() - 0.007507552643049313).abs() < 1e-12);
    }

    #[test]
    fn histograms_parsed() {
        let s = snap();
        for hist in [&s.ttft, &s.e2e, &s.inter_token] {
            assert!(hist.buckets.iter().any(|(le, _)| le.is_infinite()));
            assert_eq!(hist.get(f64::INFINITY), hist.count);
        }
        // TTFT count from the fixture.
        assert_eq!(s.ttft.count, 11008.0);
        // E2E count.
        assert_eq!(s.e2e.count, 11228.0);
        // TPOT (time_per_output_token) count.
        assert_eq!(s.inter_token.count, 7.400757e+06);
    }

    #[test]
    fn process_start_time_captured() {
        let s = snap();
        assert!(s.process_start_time.is_some());
    }

    #[test]
    fn vllm_only_fields_at_default() {
        let s = snap();
        // SGLang doesn't expose these.
        assert_eq!(s.kv_cache_usage_perc, 0.0);
        assert_eq!(s.num_preemptions_total, 0.0);
        assert_eq!(s.request_success_total, 0.0);
        assert!(s.cache_dtype.is_none());
        assert!(s.block_size.is_none());
        assert!(s.engine_awake.is_none());
        // Per-request histograms stay empty.
        assert_eq!(s.req_prompt_tokens.count, 0.0);
        assert_eq!(s.queue_time.count, 0.0);
    }
}
