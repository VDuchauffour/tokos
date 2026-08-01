//! Mock vLLM-compatible server for testing `tokos` without a real deployment.
//!
//! Serves a synthetic Prometheus `/metrics` endpoint whose counters and
//! histograms evolve as simulated requests are served, so the TUI shows live
//! activity. Also exposes a few OpenAI-style endpoints (`/v1/chat/completions`,
//! `/v1/completions`, `/v1/models`, `/health`) so the mock behaves like a
//! minimal vLLM server rather than a bare metrics emitter.
//!
//! The `/metrics` body is rendered to match what
//! [`crate::collectors::vllm::parse_metrics`] consumes: the `vllm:` prefix,
//! `engine`/`model_name` labels, `_bucket`/`_count`/`_sum` histogram triplets
//! with a literal `+Inf` bucket, plus `process_start_time_seconds`,
//! `vllm:cache_config_info`, and `vllm:engine_sleep_state`. Round-trip
//! correctness is asserted by the inline tests.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Local;
use log::{error, info};
use serde_json::json;

/// Configuration for the mock server, mirrored on the guidellm `mock-server`
/// flags so the mental model transfers. Defaults match guidellm's defaults.
#[derive(Clone, Debug)]
pub struct MockServerConfig {
    pub host: String,
    pub port: u16,
    pub model: String,
    /// Base request latency in seconds for non-streaming responses.
    pub request_latency: f64,
    pub request_latency_std: f64,
    /// Time to first token in milliseconds (stored as seconds internally).
    pub ttft_ms: f64,
    pub ttft_ms_std: f64,
    /// Inter-token latency in milliseconds.
    pub itl_ms: f64,
    pub itl_ms_std: f64,
    pub output_tokens: u32,
    pub output_tokens_std: f64,
    /// Spawn a background thread that simulates a request every
    /// `request_latency` seconds so metrics move without external traffic.
    pub auto_traffic: bool,

    /// Disable colored log output.
    pub no_color: bool,
}

impl Default for MockServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8000,
            model: "llama-3.1-8b-instruct".to_string(),
            request_latency: 3.0,
            request_latency_std: 0.0,
            ttft_ms: 150.0,
            ttft_ms_std: 0.0,
            itl_ms: 10.0,
            itl_ms_std: 0.0,
            output_tokens: 128,
            output_tokens_std: 0.0,
            auto_traffic: false,
            no_color: false,
        }
    }
}

/// Bucket upper bounds (seconds) for each latency histogram we emit. Copied
/// from the vLLM fixture so the rendered exposition text is realistic.
const TTFT_BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.02, 0.04, 0.06, 0.08, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
    20.0, 40.0, 80.0, 160.0, 640.0, 2560.0,
];
const E2E_BUCKETS: &[f64] = &[
    0.3, 0.5, 0.8, 1.0, 1.5, 2.0, 2.5, 5.0, 10.0, 15.0, 20.0, 30.0, 40.0, 50.0, 60.0, 120.0, 240.0,
    480.0, 960.0, 1920.0, 7680.0,
];
const INTER_TOKEN_BUCKETS: &[f64] = &[
    0.01, 0.025, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0, 20.0,
    40.0, 80.0,
];
const TOKEN_BUCKETS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
    50000.0,
];

/// One cumulative histogram accumulator.
#[derive(Clone, Debug)]
struct HistAcc {
    bounds: &'static [f64],
    /// Cumulative counts per `bounds` entry; the final entry is the `+Inf`
    /// bucket and equals `count`.
    buckets: Vec<f64>,
    sum: f64,
    count: f64,
}

impl HistAcc {
    fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            buckets: vec![0.0; bounds.len() + 1], // +1 for +Inf
            sum: 0.0,
            count: 0.0,
        }
    }

    /// Observe a single sample: increment every bucket whose bound >= value,
    /// plus the `+Inf` bucket; add to `sum` and `count`.
    fn observe(&mut self, value: f64) {
        let v = value.max(0.0);
        for (i, &b) in self.bounds.iter().enumerate() {
            if v <= b {
                self.buckets[i] += 1.0;
            }
        }
        self.buckets[self.bounds.len()] += 1.0; // +Inf
        self.sum += v;
        self.count += 1.0;
    }
}

/// Mutable metrics state shared between the HTTP handlers and the auto-traffic
/// thread.
struct State {
    model: String,
    process_start_time: f64,
    prompt_tokens_total: f64,
    generation_tokens_total: f64,
    prompt_tokens_cached_total: f64,
    num_preemptions_total: f64,
    prefix_cache_hits_total: f64,
    prefix_cache_queries_total: f64,
    request_success_total: f64,
    num_requests_running: f64,
    num_requests_waiting: f64,
    kv_cache_usage_perc: f64,
    ttft: HistAcc,
    inter_token: HistAcc,
    e2e: HistAcc,
    queue_time: HistAcc,
    req_prompt_tokens: HistAcc,
    req_gen_tokens: HistAcc,
    prefill_time: HistAcc,
    decode_time: HistAcc,
}

impl State {
    fn new(model: String) -> Self {
        let start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Self {
            model,
            process_start_time: start,
            prompt_tokens_total: 0.0,
            generation_tokens_total: 0.0,
            prompt_tokens_cached_total: 0.0,
            num_preemptions_total: 0.0,
            prefix_cache_hits_total: 0.0,
            prefix_cache_queries_total: 0.0,
            request_success_total: 0.0,
            num_requests_running: 0.0,
            num_requests_waiting: 0.0,
            kv_cache_usage_perc: 0.0,
            ttft: HistAcc::new(TTFT_BUCKETS),
            inter_token: HistAcc::new(INTER_TOKEN_BUCKETS),
            e2e: HistAcc::new(E2E_BUCKETS),
            queue_time: HistAcc::new(E2E_BUCKETS),
            req_prompt_tokens: HistAcc::new(TOKEN_BUCKETS),
            req_gen_tokens: HistAcc::new(TOKEN_BUCKETS),
            prefill_time: HistAcc::new(E2E_BUCKETS),
            decode_time: HistAcc::new(E2E_BUCKETS),
        }
    }
}

/// Simple deterministic pseudo-random generator (LCG) so we need no `rand`
/// crate. Seeded from the current time; produces values in `[0, 1)`.
struct Rng {
    state: u64,
}

impl Rng {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self {
            state: seed.wrapping_add(0x9e37_79b9_7f4a_7c15),
        }
    }

    /// Next float in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        // xorshift64* for decent spread.
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let x = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // Use the top 53 bits for a double.
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Sample from a normal distribution via Box-Muller, clamped to `>= 0`.
    fn normal(&mut self, mean: f64, std: f64) -> f64 {
        if std <= 0.0 {
            return mean.max(0.0);
        }
        let u1 = self.next_f64().max(1e-12);
        let u2 = self.next_f64();
        let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        (mean + z * std).max(0.0)
    }
}

/// Simulate one completed request: bump counters and record histogram
/// observations derived from the configured latency profile.
fn simulate_request(state: &Mutex<State>, config: &MockServerConfig) -> (u32, u32) {
    let mut rng = Rng::new();
    simulate_request_with(&mut rng, state, config)
}

fn simulate_request_with(
    rng: &mut Rng,
    state: &Mutex<State>,
    config: &MockServerConfig,
) -> (u32, u32) {
    let prompt_tokens = (rng.normal(256.0, 128.0) as u32).max(1);
    let output_tokens = {
        let n = rng.normal(config.output_tokens as f64, config.output_tokens_std) as u32;
        n.max(1)
    };

    let ttft_s = rng.normal(config.ttft_ms / 1000.0, config.ttft_ms_std / 1000.0);
    let itl_s = rng.normal(config.itl_ms / 1000.0, config.itl_ms_std / 1000.0);
    let decode_s = itl_s * output_tokens as f64;
    let prefill_s = (prompt_tokens as f64 * 1e-4).max(0.001);
    let queue_s = rng.normal(0.2, 0.1);
    let e2e_s = ttft_s + prefill_s + decode_s;

    let mut s = state.lock().expect("metrics state lock poisoned");
    s.prompt_tokens_total += prompt_tokens as f64;
    s.generation_tokens_total += output_tokens as f64;
    s.prefix_cache_queries_total += prompt_tokens as f64;
    // Simulate a ~30% prefix-cache hit rate.
    let hits = prompt_tokens as f64 * 0.3;
    s.prefix_cache_hits_total += hits;
    s.prompt_tokens_cached_total += hits;
    s.request_success_total += 1.0;
    // Keep KV-cache usage wobbling so the gauge chart moves.
    s.kv_cache_usage_perc = (s.kv_cache_usage_perc + rng.normal(0.05, 0.03)).clamp(0.0, 0.95);
    s.num_requests_running = rng.normal(2.0, 1.0).round().max(0.0);
    s.num_requests_waiting = rng.normal(1.0, 1.0).round().max(0.0);

    s.ttft.observe(ttft_s);
    s.inter_token.observe(itl_s);
    s.e2e.observe(e2e_s);
    s.queue_time.observe(queue_s);
    s.prefill_time.observe(prefill_s);
    s.decode_time.observe(decode_s);
    s.req_prompt_tokens.observe(prompt_tokens as f64);
    s.req_gen_tokens.observe(output_tokens as f64);

    (prompt_tokens, output_tokens)
}

fn render_metrics(state: &Mutex<State>) -> String {
    let s = state.lock().expect("metrics state lock poisoned");
    let model = &s.model;
    let lbl = format!(r#"{{engine="0",model_name="{model}"}}"#);
    let mut out = String::with_capacity(8 * 1024);

    // --- process / python bookkeeping (minimal, parser-relevant only) ---
    writeln!(
        out,
        "# HELP process_start_time_seconds Start time of the process since unix epoch in seconds."
    )
    .unwrap();
    writeln!(out, "# TYPE process_start_time_seconds gauge").unwrap();
    writeln!(
        out,
        "process_start_time_seconds {}",
        fmt_f64(s.process_start_time)
    )
    .unwrap();

    // --- scalar gauges / counters ---
    let scalars: &[(&str, f64, &str)] = &[
        ("vllm:num_requests_running", s.num_requests_running, "gauge"),
        ("vllm:num_requests_waiting", s.num_requests_waiting, "gauge"),
        ("vllm:kv_cache_usage_perc", s.kv_cache_usage_perc, "gauge"),
        ("vllm:prompt_tokens_total", s.prompt_tokens_total, "counter"),
        (
            "vllm:generation_tokens_total",
            s.generation_tokens_total,
            "counter",
        ),
        (
            "vllm:prompt_tokens_cached_total",
            s.prompt_tokens_cached_total,
            "counter",
        ),
        (
            "vllm:num_preemptions_total",
            s.num_preemptions_total,
            "counter",
        ),
        (
            "vllm:prefix_cache_hits_total",
            s.prefix_cache_hits_total,
            "counter",
        ),
        (
            "vllm:prefix_cache_queries_total",
            s.prefix_cache_queries_total,
            "counter",
        ),
        (
            "vllm:request_success_total",
            s.request_success_total,
            "counter",
        ),
    ];
    for &(name, value, ty) in scalars {
        writeln!(out, "# HELP {name} mock vLLM metric.").unwrap();
        writeln!(out, "# TYPE {name} {ty}").unwrap();
        writeln!(out, "{name}{lbl} {}", fmt_f64(value)).unwrap();
    }

    // --- request_success_total is split by finished_reason in real vLLM; the
    //     parser sums across labels, so emit one line per reason for realism. ---
    // (Already emitted the summed total above; skip per-reason to avoid double
    // counting, since `parse_metrics` sums all lines with the same name.)

    // --- histograms ---
    let hists: &[(&str, &HistAcc, &str)] = &[
        (
            "vllm:time_to_first_token_seconds",
            &s.ttft,
            "time to first token in seconds.",
        ),
        (
            "vllm:inter_token_latency_seconds",
            &s.inter_token,
            "inter-token latency in seconds.",
        ),
        (
            "vllm:e2e_request_latency_seconds",
            &s.e2e,
            "e2e request latency in seconds.",
        ),
        (
            "vllm:request_queue_time_seconds",
            &s.queue_time,
            "time spent in WAITING phase for request.",
        ),
        (
            "vllm:request_prompt_tokens",
            &s.req_prompt_tokens,
            "prefill tokens per request.",
        ),
        (
            "vllm:request_generation_tokens",
            &s.req_gen_tokens,
            "generation tokens per request.",
        ),
        (
            "vllm:request_prefill_time_seconds",
            &s.prefill_time,
            "time spent in PREFILL phase for request.",
        ),
        (
            "vllm:request_decode_time_seconds",
            &s.decode_time,
            "time spent in DECODE phase for request.",
        ),
    ];
    for &(base, acc, help) in hists {
        writeln!(out, "# HELP {base} {help}").unwrap();
        writeln!(out, "# TYPE {base} histogram").unwrap();
        for (i, &le) in acc.bounds.iter().enumerate() {
            writeln!(
                out,
                r#"{base}_bucket{{engine="0",le="{le}",model_name="{model}"}} {}"#,
                fmt_f64(acc.buckets[i])
            )
            .unwrap();
        }
        writeln!(
            out,
            r#"{base}_bucket{{engine="0",le="+Inf",model_name="{model}"}} {}"#,
            fmt_f64(acc.buckets[acc.bounds.len()])
        )
        .unwrap();
        writeln!(out, r#"{base}_count{lbl} {}"#, fmt_f64(acc.count)).unwrap();
        writeln!(out, r#"{base}_sum{lbl} {}"#, fmt_f64(acc.sum)).unwrap();
    }

    // --- engine config / sleep state (info-style gauges) ---
    writeln!(
        out,
        "# HELP vllm:cache_config_info Information of the LLMEngine CacheConfig"
    )
    .unwrap();
    writeln!(out, "# TYPE vllm:cache_config_info gauge").unwrap();
    writeln!(
        out,
        r#"vllm:cache_config_info{{block_size="16",cache_dtype="auto",enable_prefix_caching="False",engine="0",gpu_memory_utilization="0.9",model_name="{model}",num_gpu_blocks="1024"}} 1.0"#
    )
    .unwrap();

    writeln!(out, "# HELP vllm:engine_sleep_state Engine sleep state.").unwrap();
    writeln!(out, "# TYPE vllm:engine_sleep_state gauge").unwrap();
    writeln!(
        out,
        r#"vllm:engine_sleep_state{{engine="0",model_name="{model}",sleep_state="awake"}} 1.0"#
    )
    .unwrap();

    out
}

/// Initialize the global logger with a local-time timestamp format. Log
/// levels are colorized via `env_logger`'s default level styles; pass
/// `no_color = true` to emit plain (uncolored) output.
fn init_logger(no_color: bool) {
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .format(|buf, record| {
            let level_style = buf.default_level_style(record.level());
            writeln!(
                buf,
                "[{}] {level_style}{}{level_style:#} - {}",
                Local::now().format("%H:%M:%S"),
                record.level(),
                record.args()
            )
        });
    if no_color {
        builder.write_style(env_logger::WriteStyle::Never);
    }
    builder.init();
}

/// Format an f64 the way Prometheus exposition text expects (plain decimal,
/// never `inf`/`nan` literals for finite values). `+Inf`/`NaN` are not emitted
/// for values, only for the `le` label which is rendered by the caller.
fn fmt_f64(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        };
    }
    // `{:?}` yields a round-trippable representation; for typical metric
    // magnitudes it's fine and avoids scientific-notation surprises.
    format!("{v:?}")
}

/// Start the mock server. Blocks until the listener errors or the process is
/// interrupted.
pub fn run(config: MockServerConfig) -> std::process::ExitCode {
    init_logger(config.no_color);
    let addr = format!("{}:{}", config.host, config.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            error!("failed to bind {addr}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let state = Arc::new(Mutex::new(State::new(config.model.clone())));
    let stop = Arc::new(AtomicBool::new(false));

    if config.auto_traffic {
        let st = Arc::clone(&state);
        let cfg = config.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut rng = Rng::new();
            let interval = cfg.request_latency.max(0.1);
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs_f64(interval));
                let (prompt, output) = simulate_request_with(&mut rng, &st, &cfg);
                info!("auto-traffic request (prompt={prompt}, output={output})");
            }
        });
    }

    info!(
        "listening on http://{} (model={}, auto_traffic={})",
        addr, config.model, config.auto_traffic
    );

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let st = Arc::clone(&state);
        let cfg = config.clone();
        thread::spawn(move || {
            // Best-effort: ignore per-connection errors.
            let _ = handle_connection(stream, &st, &cfg);
        });
    }

    stop.store(true, Ordering::Relaxed);
    std::process::ExitCode::SUCCESS
}

/// Handle a single HTTP/1.1 connection.
fn handle_connection(
    mut stream: TcpStream,
    state: &Arc<Mutex<State>>,
    config: &MockServerConfig,
) -> std::io::Result<()> {
    // Read the request line + headers (up to \r\n\r\n), cap at 64 KiB.
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // Collect content-length for POST bodies.
    let content_length: usize = text
        .lines()
        .find_map(|l| {
            let l = l.to_ascii_lowercase();
            l.strip_prefix("content-length:")
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(0);

    // If we haven't read the whole body yet, read the remainder (best-effort).
    if content_length > 0 && method == "POST" {
        let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
        if let Some(h) = header_end {
            let already = buf.len().saturating_sub(h + 4);
            let mut want = content_length.saturating_sub(already);
            while want > 0 {
                let len = tmp.len().min(want);
                let n = stream.read(&mut tmp[..len])?;
                if n == 0 {
                    break;
                }
                want -= n;
            }
        }
    }

    let (status, ctype, body, endpoint) = route(method, path, state, config);
    info!("{method} {path} -> {status} [{endpoint}]");
    write_response(&mut stream, status, ctype, &body)
}

fn route(
    method: &str,
    path: &str,
    state: &Arc<Mutex<State>>,
    config: &MockServerConfig,
) -> (u16, &'static str, String, &'static str) {
    if method == "OPTIONS" {
        return (204, "text/plain", String::new(), "cors_preflight");
    }
    match (method, path) {
        ("GET", "/metrics") => {
            let body = render_metrics(state);
            (200, "text/plain; version=0.0.4", body, "metrics")
        }
        ("GET", "/health") => (
            200,
            "application/json",
            json!({"status": "healthy"}).to_string(),
            "health",
        ),
        ("GET", "/v1/models") => (
            200,
            "application/json",
            json!({
                "object": "list",
                "data": [{
                    "id": config.model,
                    "object": "model",
                    "owned_by": "tokos-mock"
                }]
            })
            .to_string(),
            "models",
        ),
        ("POST", "/v1/chat/completions") | ("POST", "/v1/completions") => {
            // Simulate inference latency, then update metrics.
            thread::sleep(Duration::from_secs_f64(config.request_latency.max(0.0)));
            let (prompt_tokens, output_tokens) = simulate_request(state, config);
            (
                200,
                "application/json",
                json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "model": config.model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "This is a mock completion from the tokos mock server."},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": output_tokens,
                        "total_tokens": prompt_tokens + output_tokens
                    }
                })
                .to_string(),
                "completions",
            )
        }
        _ => (
            404,
            "application/json",
            json!({"error": {"message": "Not Found", "type": "not_found_error"}}).to_string(),
            "not_found",
        ),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        404 => "Not Found",
        _ => "OK",
    };
    let mut header = format!("HTTP/1.1 {status} {reason}\r\n");
    header.push_str(&format!("Content-Type: {content_type}\r\n"));
    header.push_str(&format!("Content-Length: {}\r\n", body.len()));
    header.push_str("Access-Control-Allow-Origin: *\r\n");
    header.push_str("Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n");
    header.push_str("Access-Control-Allow-Headers: Content-Type, Authorization\r\n");
    header.push_str("Server: tokos-mock\r\n");
    header.push_str("Connection: close\r\n\r\n");
    stream.write_all(header.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::vllm::parse_metrics;

    fn state_with(model: &str) -> Arc<Mutex<State>> {
        Arc::new(Mutex::new(State::new(model.to_string())))
    }

    #[test]
    fn empty_state_round_trips() {
        let st = state_with("llama-3.1-8b-instruct");
        let text = render_metrics(&st);
        let snap = parse_metrics(&text);
        assert!(snap.reachable);
        assert_eq!(snap.model_name.as_deref(), Some("llama-3.1-8b-instruct"));
        assert_eq!(snap.request_success_total, 0.0);
        assert_eq!(snap.prompt_tokens_total, 0.0);
        assert_eq!(snap.generation_tokens_total, 0.0);
        for h in [
            &snap.ttft,
            &snap.inter_token,
            &snap.e2e,
            &snap.queue_time,
            &snap.req_prompt_tokens,
            &snap.req_gen_tokens,
            &snap.prefill_time,
            &snap.decode_time,
        ] {
            assert_eq!(h.count, 0.0);
            assert_eq!(h.get(f64::INFINITY), 0.0);
        }
        assert_eq!(snap.engine_awake, Some(true));
        assert!(snap.cache_dtype.is_some());
        assert!(snap.process_start_time.is_some());
    }

    #[test]
    fn simulated_requests_round_trip() {
        let cfg = MockServerConfig {
            request_latency: 0.0,
            ttft_ms: 150.0,
            itl_ms: 10.0,
            output_tokens: 128,
            ..MockServerConfig::default()
        };
        let st = state_with(&cfg.model.clone());
        // Drive several simulated requests.
        let mut rng = Rng::new();
        for _ in 0..10 {
            simulate_request_with(&mut rng, &st, &cfg);
        }
        let text = render_metrics(&st);
        let snap = parse_metrics(&text);

        assert!(snap.reachable);
        assert_eq!(snap.model_name.as_deref(), Some("llama-3.1-8b-instruct"));
        assert!(snap.request_success_total > 0.0);
        assert!(snap.prompt_tokens_total > 0.0);
        assert!(snap.generation_tokens_total > 0.0);

        // Every histogram must have count > 0 and +Inf bucket == count.
        for h in [
            &snap.ttft,
            &snap.inter_token,
            &snap.e2e,
            &snap.queue_time,
            &snap.req_prompt_tokens,
            &snap.req_gen_tokens,
            &snap.prefill_time,
            &snap.decode_time,
        ] {
            assert!(h.count > 0.0, "histogram count should be > 0");
            assert_eq!(
                h.get(f64::INFINITY),
                h.count,
                "+Inf bucket must equal count"
            );
        }

        assert_eq!(snap.engine_awake, Some(true));
        assert_eq!(snap.cache_dtype.as_deref(), Some("auto"));
        assert_eq!(snap.block_size.as_deref(), Some("16"));
    }

    #[test]
    fn rng_produces_sane_normals() {
        let mut rng = Rng::new();
        let mut sum = 0.0;
        for _ in 0..1000 {
            let v = rng.normal(5.0, 1.0);
            assert!(v >= 0.0);
            sum += v;
        }
        let mean = sum / 1000.0;
        assert!(
            (mean - 5.0).abs() < 1.0,
            "normal mean should be near 5, got {mean}"
        );
    }
}
