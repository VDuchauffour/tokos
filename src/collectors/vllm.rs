//! Collect a [`BackendSnapshot`] from a vLLM `/metrics` endpoint.
//!
//! [`parse_metrics`] is a pure function (no I/O) so it can be unit-tested
//! against a fixture. Values are summed across `engine` labels in case of
//! multiple engines.

use std::collections::HashMap;
use std::time::Duration;

use tracing::instrument;

use crate::collectors::common::{self, le_to_float};
use crate::state::{BackendSnapshot, Histogram};

/// Scalar series we read directly by sample name (summed across engine labels).
const SCALAR_NAMES: &[&str] = &[
    "vllm:generation_tokens_total",
    "vllm:prompt_tokens_total",
    "vllm:prompt_tokens_cached_total",
    "vllm:num_preemptions_total",
    "vllm:prefix_cache_hits_total",
    "vllm:prefix_cache_queries_total",
    "vllm:num_requests_running",
    "vllm:num_requests_waiting",
    "vllm:kv_cache_usage_perc",
    "vllm:request_success_total",
];

/// Histogram base names -> attribute on [`BackendSnapshot`].
const HISTOGRAMS: &[(&str, &str)] = &[
    ("vllm:time_to_first_token_seconds", "ttft"),
    ("vllm:inter_token_latency_seconds", "inter_token"),
    ("vllm:e2e_request_latency_seconds", "e2e"),
    ("vllm:request_queue_time_seconds", "queue_time"),
    ("vllm:request_prompt_tokens", "req_prompt_tokens"),
    ("vllm:request_generation_tokens", "req_gen_tokens"),
    ("vllm:request_prefill_time_seconds", "prefill_time"),
    ("vllm:request_decode_time_seconds", "decode_time"),
];

/// Parse Prometheus exposition text into a [`BackendSnapshot`].
#[instrument(skip(text), fields(bytes = text.len()))]
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

        // CacheConfig is exposed as a single info gauge whose labels carry the
        // interesting config (KV-cache dtype, block size, etc.).
        if name == "vllm:cache_config_info" {
            for (k, v) in &s.labels {
                match *k {
                    "cache_dtype" => snap.cache_dtype = Some(v.clone()),
                    "block_size" => snap.block_size = Some(v.clone()),
                    "num_gpu_blocks" => snap.num_gpu_blocks = Some(v.clone()),
                    "gpu_memory_utilization" => snap.gpu_memory_utilization = Some(v.clone()),
                    "enable_prefix_caching" => {
                        snap.enable_prefix_caching = Some(*v == "True");
                    }
                    _ => {}
                }
            }
            continue;
        }

        // Engine sleep state: awake=1 means the engine is serving.
        if name == "vllm:engine_sleep_state" {
            if s.labels
                .iter()
                .any(|(k, v)| *k == "sleep_state" && v == "awake")
            {
                snap.engine_awake = Some(value == 1.0);
            }
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

    snap.generation_tokens_total = *scalars.get("vllm:generation_tokens_total").unwrap_or(&0.0);
    snap.prompt_tokens_total = *scalars.get("vllm:prompt_tokens_total").unwrap_or(&0.0);
    snap.prompt_tokens_cached_total = *scalars
        .get("vllm:prompt_tokens_cached_total")
        .unwrap_or(&0.0);
    snap.num_preemptions_total = *scalars.get("vllm:num_preemptions_total").unwrap_or(&0.0);
    snap.prefix_cache_hits_total = *scalars.get("vllm:prefix_cache_hits_total").unwrap_or(&0.0);
    snap.prefix_cache_queries_total = *scalars
        .get("vllm:prefix_cache_queries_total")
        .unwrap_or(&0.0);
    snap.num_requests_running = *scalars.get("vllm:num_requests_running").unwrap_or(&0.0);
    snap.num_requests_waiting = *scalars.get("vllm:num_requests_waiting").unwrap_or(&0.0);
    snap.kv_cache_usage_perc = *scalars.get("vllm:kv_cache_usage_perc").unwrap_or(&0.0);
    snap.request_success_total = *scalars.get("vllm:request_success_total").unwrap_or(&0.0);

    // Move histograms out of the map onto the snapshot by attribute.
    for &(base, attr) in HISTOGRAMS {
        if let Some(hist) = hists.remove(base) {
            match attr {
                "ttft" => snap.ttft = hist,
                "inter_token" => snap.inter_token = hist,
                "e2e" => snap.e2e = hist,
                "queue_time" => snap.queue_time = hist,
                "req_prompt_tokens" => snap.req_prompt_tokens = hist,
                "req_gen_tokens" => snap.req_gen_tokens = hist,
                "prefill_time" => snap.prefill_time = hist,
                "decode_time" => snap.decode_time = hist,
                _ => {}
            }
        }
    }

    snap
}

/// Fetches and parses `/metrics`, returning a snapshot each poll.
pub struct VllmCollector {
    metrics_url: String,
    timeout: Duration,
}

impl VllmCollector {
    pub fn new(metrics_url: String, timeout: f64) -> Self {
        Self {
            metrics_url,
            timeout: Duration::from_secs_f64(timeout.max(0.001)),
        }
    }

    #[instrument(skip(self), fields(url = %self.metrics_url))]
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

impl crate::collectors::Backend for VllmCollector {
    fn poll(&self) -> BackendSnapshot {
        VllmCollector::poll(self)
    }

    fn effective_kind(&self) -> crate::collectors::BackendKind {
        crate::collectors::BackendKind::Vllm
    }
}

// ---- fixture-driven tests (port of tests/test_prometheus.py) ----
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/metrics_fixture.txt");

    fn snap() -> BackendSnapshot {
        parse_metrics(FIXTURE)
    }

    #[test]
    fn reachable_and_model() {
        let s = snap();
        assert!(s.reachable);
        assert_eq!(s.model_name.as_deref(), Some("Qwen/Qwen3.6-35B-A3B"));
    }

    #[test]
    fn scalar_values() {
        let s = snap();
        assert_eq!(s.num_requests_running, 0.0);
        assert_eq!(s.num_requests_waiting, 0.0);
        assert_eq!(s.kv_cache_usage_perc, 0.0);
        assert_eq!(s.prompt_tokens_total, 57351109.0);
        assert_eq!(s.generation_tokens_total, 1799939.0);
        assert_eq!(s.num_preemptions_total, 0.0);
    }

    #[test]
    fn histograms_parsed() {
        let s = snap();
        for hist in [&s.ttft, &s.inter_token, &s.e2e, &s.queue_time] {
            assert!(hist.buckets.iter().any(|(le, _)| le.is_infinite()));
            assert_eq!(hist.get(f64::INFINITY), hist.count);
        }
    }

    #[test]
    fn prefix_cache_hit_rate_guarded() {
        let s = snap();
        assert_eq!(s.prefix_cache_hit_rate(), 0.0);
    }

    #[test]
    fn engine_info_parsed() {
        let s = snap();
        assert_eq!(s.cache_dtype.as_deref(), Some("fp8"));
        assert_eq!(s.block_size.as_deref(), Some("16"));
        assert_eq!(s.num_gpu_blocks.as_deref(), Some("144"));
        assert_eq!(s.gpu_memory_utilization.as_deref(), Some("0.88"));
        assert_eq!(s.enable_prefix_caching, Some(false));
        assert!((s.process_start_time.unwrap() - 1.77995408251e9).abs() < 1.0);
        assert_eq!(s.engine_awake, Some(true));
        assert_eq!(s.request_success_total, 12218.0);
    }
}
