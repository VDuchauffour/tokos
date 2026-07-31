//! Collect a [`VllmSnapshot`] from a vLLM `/metrics` endpoint.
//!
//! [`parse_metrics`] is a pure function (no I/O) so it can be unit-tested
//! against a fixture. Values are summed across `engine` labels in case of
//! multiple engines.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use crate::state::{Histogram, VllmSnapshot};

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

/// Cap on a single /metrics body. The endpoint is plain text and normally well
/// under 1 MB; this bounds memory if a compromised/misconfigured server (or a
/// MITM on plain HTTP) streams an unbounded body.
pub const MAX_METRICS_BYTES: usize = 16 * 1024 * 1024;

/// Histogram base names -> attribute on [`VllmSnapshot`].
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

fn le_to_float(le: &str) -> f64 {
    match le.trim() {
        "+Inf" | "Inf" | "inf" => f64::INFINITY,
        s => s.parse::<f64>().unwrap_or(f64::INFINITY),
    }
}

fn parse_value(s: &str) -> f64 {
    match s.trim() {
        "+Inf" | "Inf" | "inf" | "Infinity" | "infinity" => f64::INFINITY,
        "-Inf" | "-inf" => f64::NEG_INFINITY,
        "NaN" | "nan" => f64::NAN,
        other => other.parse::<f64>().unwrap_or(0.0),
    }
}

/// One parsed metric sample: name, labels, value.
struct Sample<'a> {
    name: &'a str,
    labels: Vec<(&'a str, String)>,
    value: f64,
}

/// Parse a single exposition-text line into a [`Sample`], or `None` for
/// comments / blank lines.
fn parse_line(line: &str) -> Option<Sample<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Metric name: [a-zA-Z_:][a-zA-Z0-9_:]*
    let name_end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(line.len());
    let (name, rest) = line.split_at(name_end);
    if name.is_empty() {
        return None;
    }

    // Optional label block {k="v",...}
    let mut rest = rest.trim_start();
    let mut labels: Vec<(&str, String)> = Vec::new();
    if rest.starts_with('{') {
        rest = &rest[1..];
        loop {
            rest = rest.trim_start();
            if rest.starts_with('}') {
                rest = &rest[1..];
                break;
            }
            if rest.is_empty() {
                break;
            }
            // key
            let k_end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let (key, after) = rest.split_at(k_end);
            rest = after.trim_start();
            if !rest.starts_with('=') {
                break;
            }
            rest = rest[1..].trim_start();
            if !rest.starts_with('"') {
                break;
            }
            // Read a quoted string with escapes (`\"` `\\` `\n`), stopping at
            // the closing `"`. Operate on bytes but push whole chars so UTF-8
            // prompts survive intact.
            rest = &rest[1..];
            let mut val = String::new();
            let mut i = 0;
            let bytes = rest.as_bytes();
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'\\' {
                    i += 1;
                    if i < bytes.len() {
                        match bytes[i] {
                            b'"' => val.push('"'),
                            b'\\' => val.push('\\'),
                            b'n' => val.push('\n'),
                            other => val.push(other as char),
                        }
                        i += 1;
                    }
                } else if b == b'"' {
                    i += 1;
                    break;
                } else {
                    let ch = rest[i..].chars().next().unwrap();
                    val.push(ch);
                    i += ch.len_utf8();
                }
            }
            rest = &rest[i..];
            labels.push((key, val));
            rest = rest.trim_start();
            if rest.starts_with(',') {
                rest = &rest[1..];
            } else if rest.starts_with('}') {
                rest = &rest[1..];
                break;
            } else {
                break;
            }
        }
    }

    // value (first whitespace-delimited token; ignore optional timestamp)
    let value_token = rest.split_whitespace().next().unwrap_or("");
    if value_token.is_empty() {
        return None;
    }
    let value = parse_value(value_token);
    Some(Sample {
        name,
        labels,
        value,
    })
}

/// Parse Prometheus exposition text into a [`VllmSnapshot`].
pub fn parse_metrics(text: &str) -> VllmSnapshot {
    let mut snap = VllmSnapshot {
        reachable: true,
        ..VllmSnapshot::default()
    };

    let mut scalars: HashMap<&str, f64> = SCALAR_NAMES.iter().map(|&n| (n, 0.0)).collect();
    let mut hists: HashMap<&str, Histogram> = HISTOGRAMS
        .iter()
        .map(|&(b, _)| (b, Histogram::new()))
        .collect();

    for line in text.lines() {
        let Some(s) = parse_line(line) else {
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

    pub fn poll(&self) -> VllmSnapshot {
        let resp = ureq::get(&self.metrics_url)
            .set("Accept", "text/plain")
            .timeout(self.timeout)
            .call();
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                return VllmSnapshot {
                    reachable: false,
                    error: Some(short_error(&e)),
                    ..VllmSnapshot::default()
                };
            }
        };

        // Read the body with a cap so a runaway server can't exhaust memory.
        let mut reader = resp.into_reader().take((MAX_METRICS_BYTES as u64) + 1);
        let mut buf = Vec::new();
        if let Err(e) = reader.read_to_end(&mut buf) {
            return VllmSnapshot {
                reachable: false,
                error: Some(format!("{e}")),
                ..VllmSnapshot::default()
            };
        }
        if buf.len() > MAX_METRICS_BYTES {
            return VllmSnapshot {
                reachable: false,
                error: Some(format!("/metrics body exceeded {MAX_METRICS_BYTES} bytes")),
                ..VllmSnapshot::default()
            };
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        parse_metrics(&text)
    }
}

fn short_error(e: &ureq::Error) -> String {
    // ureq splits errors into Status (non-2xx) and Transport; surface a short
    // reason for the disconnect banner.
    match e {
        ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => {
            let msg = t.to_string();
            // "Transport: ..." -> strip the prefix for a terser banner.
            msg.strip_prefix("Transport: ").unwrap_or(&msg).to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blank() {
        let s = parse_line("# HELP foo bar");
        assert!(s.is_none());
        let s = parse_line("   ");
        assert!(s.is_none());
    }

    #[test]
    fn parse_plain_value() {
        let s = parse_line("process_start_time_seconds 1.77995408251e+09").unwrap();
        assert_eq!(s.name, "process_start_time_seconds");
        assert!((s.value - 1.77995408251e9).abs() < 1e-3);
        assert!(s.labels.is_empty());
    }

    #[test]
    fn parse_labels_and_inf() {
        let s = parse_line(r#"vllm:time_to_first_token_seconds_bucket{le="+Inf",engine="0"} 42.0"#)
            .unwrap();
        assert_eq!(s.name, "vllm:time_to_first_token_seconds_bucket");
        assert_eq!(s.value, 42.0);
        let le = s.labels.iter().find(|(k, _)| *k == "le").unwrap();
        assert_eq!(le.1, "+Inf");
    }

    #[test]
    fn parse_escaped_quotes() {
        let s = parse_line(r#"x{prompt="he said \"hi\"\n"} 1.0"#).unwrap();
        let p = s.labels.iter().find(|(k, _)| *k == "prompt").unwrap();
        assert_eq!(p.1, "he said \"hi\"\n");
    }

    // ---- fixture-driven tests (port of tests/test_prometheus.py) ----
    const FIXTURE: &str = include_str!("../../tests/metrics_fixture.txt");

    fn snap() -> VllmSnapshot {
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
