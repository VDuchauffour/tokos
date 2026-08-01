//! Snapshot structs, ring-buffer series, and rate/histogram math.
//!
//! The collectors produce *raw* snapshots (current counter/gauge values). The
//! UI thread feeds successive snapshots into a [`History`], which turns counters
//! into rates and histograms into recent-average latencies, appending the
//! derived values to per-series ring buffers for charting.

use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Fixed-length ring buffer of floats for chart history.
pub struct Series {
    buf: VecDeque<f64>,
    maxlen: usize,
}

impl Series {
    pub fn new(maxlen: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(maxlen + 1),
            maxlen,
        }
    }

    pub fn append(&mut self, value: f64) {
        self.buf.push_back(value);
        if self.buf.len() > self.maxlen {
            self.buf.pop_front();
        }
    }

    pub fn values(&self) -> Vec<f64> {
        self.buf.iter().copied().collect()
    }

    pub fn last(&self) -> f64 {
        self.buf.back().copied().unwrap_or(0.0)
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// A Prometheus histogram's cumulative state at one point in time.
///
/// `buckets` holds `(upper_bound, cumulative_count)` pairs kept sorted by
/// `upper_bound`; `+Inf` is stored as `f64::INFINITY`.
#[derive(Clone, Debug)]
pub struct Histogram {
    pub count: f64,
    pub sum: f64,
    pub buckets: Vec<(f64, f64)>,
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            count: 0.0,
            sum: 0.0,
            buckets: Vec::new(),
        }
    }

    pub fn get(&self, le: f64) -> f64 {
        self.buckets
            .iter()
            .find(|(k, _)| *k == le)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    }

    /// Add `v` to the bucket whose upper bound is `le`, inserting (and
    /// re-sorting) if it doesn't exist.
    pub fn add(&mut self, le: f64, v: f64) {
        if let Some(e) = self.buckets.iter_mut().find(|(k, _)| *k == le) {
            e.1 += v;
        } else {
            self.buckets.push((le, v));
            self.buckets
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        }
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Raw current values pulled from one /metrics scrape.
#[derive(Clone, Debug)]
pub struct BackendSnapshot {
    pub reachable: bool,
    pub error: Option<String>,
    pub model_name: Option<String>,

    // Engine / config info (from labels on info-style metrics).
    pub process_start_time: Option<f64>, // unix epoch; for uptime
    pub cache_dtype: Option<String>,
    pub block_size: Option<String>,
    pub gpu_memory_utilization: Option<String>, // configured target, e.g. "0.88"
    pub num_gpu_blocks: Option<String>,
    pub enable_prefix_caching: Option<bool>,
    pub engine_awake: Option<bool>,

    // Counters
    pub generation_tokens_total: f64,
    pub prompt_tokens_total: f64,
    pub prompt_tokens_cached_total: f64,
    pub num_preemptions_total: f64,
    pub prefix_cache_hits_total: f64,
    pub prefix_cache_queries_total: f64,
    pub request_success_total: f64, // summed across finish reasons

    // Gauges
    pub num_requests_running: f64,
    pub num_requests_waiting: f64,
    pub kv_cache_usage_perc: f64,

    // Latency histograms
    pub ttft: Histogram,
    pub inter_token: Histogram,
    pub e2e: Histogram,
    pub queue_time: Histogram,

    // Per-request size / phase-timing histograms (observed once per completed
    // request). Used for the requests panel's per-request averages list.
    pub req_prompt_tokens: Histogram,
    pub req_gen_tokens: Histogram,
    pub prefill_time: Histogram,
    pub decode_time: Histogram,
}

impl BackendSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cumulative prefix-cache hit rate as a fraction in [0, 1].
    pub fn prefix_cache_hit_rate(&self) -> f64 {
        if self.prefix_cache_queries_total <= 0.0 {
            return 0.0;
        }
        self.prefix_cache_hits_total / self.prefix_cache_queries_total
    }
}

impl Default for BackendSnapshot {
    fn default() -> Self {
        Self {
            reachable: false,
            error: None,
            model_name: None,
            process_start_time: None,
            cache_dtype: None,
            block_size: None,
            gpu_memory_utilization: None,
            num_gpu_blocks: None,
            enable_prefix_caching: None,
            engine_awake: None,
            generation_tokens_total: 0.0,
            prompt_tokens_total: 0.0,
            prompt_tokens_cached_total: 0.0,
            num_preemptions_total: 0.0,
            prefix_cache_hits_total: 0.0,
            prefix_cache_queries_total: 0.0,
            request_success_total: 0.0,
            num_requests_running: 0.0,
            num_requests_waiting: 0.0,
            kv_cache_usage_perc: 0.0,
            ttft: Histogram::new(),
            inter_token: Histogram::new(),
            e2e: Histogram::new(),
            queue_time: Histogram::new(),
            req_prompt_tokens: Histogram::new(),
            req_gen_tokens: Histogram::new(),
            prefill_time: Histogram::new(),
            decode_time: Histogram::new(),
        }
    }
}

/// One row in the request feed.
///
/// Produced from a vLLM request-log line (`--enable-log-requests`), carrying
/// `request_id`, `max_tokens` and optionally `prompt` (vLLM >= 0.11.3). The
/// `path` (endpoint) is inferred from the request-id prefix. `status` is
/// `None` because the request-log line is emitted at arrival, before
/// completion.
#[derive(Clone, Debug)]
pub struct MergedLogEntry {
    pub t: f64, // epoch seconds (when observed)
    pub client: String,
    pub method: String,
    pub path: String,
    pub status: Option<i32>,
    pub request_id: Option<String>,
    pub max_tokens: Option<i64>,
    pub prompt: Option<String>,
    pub prompt_chars: Option<i64>, // logged prompt length in characters
}

impl MergedLogEntry {
    pub fn new(t: f64) -> Self {
        Self {
            t,
            client: "—".to_string(),
            method: "POST".to_string(),
            path: String::new(),
            status: None,
            request_id: None,
            max_tokens: None,
            prompt: None,
            prompt_chars: None,
        }
    }

    pub fn ok(&self) -> bool {
        matches!(self.status, Some(s) if (200..400).contains(&s))
    }
}

/// A combined vLLM + log sample taken at one monotonic instant.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub monotonic: f64,
    pub backend: BackendSnapshot,
    pub merged_log: Vec<MergedLogEntry>,
    pub access_error: Option<String>,
}

impl Snapshot {
    pub fn new(monotonic: f64, backend: BackendSnapshot) -> Self {
        Self {
            monotonic,
            backend,
            merged_log: Vec::new(),
            access_error: None,
        }
    }
}

/// Monotonic seconds since the first call (process-local clock).
pub fn monotonic_now() -> f64 {
    static CLOCK_START: OnceLock<Instant> = OnceLock::new();
    let start = CLOCK_START.get_or_init(Instant::now);
    Instant::now().duration_since(*start).as_secs_f64()
}

/// Unix-epoch seconds (wall clock) — used for request age and uptime.
pub fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `Δvalue/Δt` for a monotonic counter.
///
/// Guards against non-positive `Δt` (returns 0) and counter resets — if the
/// value decreased (server restart), treat the rate as 0 rather than negative.
pub fn compute_rate(prev_value: f64, prev_t: f64, cur_value: f64, cur_t: f64) -> f64 {
    let dt = cur_t - prev_t;
    if dt <= 0.0 {
        return 0.0;
    }
    let dv = cur_value - prev_value;
    if dv < 0.0 {
        return 0.0;
    }
    dv / dt
}

/// Recent mean = `Δsum / Δcount` between two scrapes.
pub fn histogram_recent_avg(prev: &Histogram, cur: &Histogram) -> f64 {
    let dcount = cur.count - prev.count;
    let dsum = cur.sum - prev.sum;
    if dcount <= 0.0 || dsum < 0.0 {
        return 0.0;
    }
    dsum / dcount
}

/// Mean observation, recent if possible, else cumulative.
///
/// Like [`histogram_recent_avg`] but falls back to the all-time mean
/// (`sum/count`) when no new observations landed in the window. Per-request
/// histograms only update when a request *completes*, so a pure recent average
/// would flicker to 0 between completions; this keeps it stable.
pub fn histogram_avg(prev: &Histogram, cur: &Histogram) -> f64 {
    let dcount = cur.count - prev.count;
    let dsum = cur.sum - prev.sum;
    if dcount > 0.0 && dsum >= 0.0 {
        return dsum / dcount;
    }
    if cur.count > 0.0 {
        return cur.sum / cur.count;
    }
    0.0
}

/// Approximate quantile `q` (0..1) of observations in the (prev, cur) window.
///
/// Uses the per-bucket count deltas and linear interpolation within the
/// containing bucket. Returns 0 if no observations occurred.
pub fn histogram_quantile(prev: &Histogram, cur: &Histogram, q: f64) -> f64 {
    if cur.buckets.is_empty() {
        return 0.0;
    }
    let delta_total = cur.count - prev.count;
    if delta_total <= 0.0 {
        return 0.0;
    }

    let target = q * delta_total;
    let mut prev_cum = 0.0;
    let mut cur_cum = 0.0;
    let mut lower_bound = 0.0;
    for &(le, _) in &cur.buckets {
        let mut d = cur.get(le) - prev.get(le);
        if d < 0.0 {
            d = 0.0;
        }
        cur_cum += d;
        if cur_cum >= target {
            if le.is_infinite() {
                return lower_bound;
            }
            let span = cur_cum - prev_cum;
            if span <= 0.0 {
                return le;
            }
            let frac = (target - prev_cum) / span;
            return lower_bound + frac * (le - lower_bound);
        }
        prev_cum = cur_cum;
        if !le.is_infinite() {
            lower_bound = le;
        }
    }
    lower_bound
}

/// Names of the series kept, for iteration in tests/UI.
pub const SERIES_NAMES: &[&str] = &[
    "gen_tok_s",
    "prompt_tok_s",
    "running",
    "waiting",
    "kv_cache",
    "ttft",
    "tpot",
    "e2e",
    "queue_time",
    "req_prompt_tok",
    "req_gen_tok",
    "req_prefill",
    "req_decode",
];

/// Time constants (seconds) for the 1/5/15-minute windowed averages. EMA decay
/// matches Unix loadavg.
pub const WINDOW_TAUS: [f64; 3] = [60.0, 300.0, 900.0];

/// Holds per-series ring buffers and derives rates from raw snapshots.
///
/// Call [`History::update`] with each new [`Snapshot`]; it computes derived
/// quantities against the previous snapshot and appends them to the series.
/// Alongside the raw series it keeps load-average-style exponential moving
/// averages over three time horizons (1/5/15 minutes), in `avg`.
pub struct History {
    pub maxlen: usize,
    pub series: HashMap<&'static str, Series>,
    pub derived: HashMap<&'static str, f64>,
    /// 1/5/15-minute EMAs per series: `avg[name] == [ema_1m, ema_5m, ema_15m]`.
    pub avg: HashMap<&'static str, [f64; 3]>,
    prev: Option<Snapshot>,
    avg_seeded: bool,
    avg_t: Option<f64>,
}

impl History {
    pub fn new(maxlen: usize) -> Self {
        let mut series = HashMap::new();
        let mut derived = HashMap::new();
        let mut avg = HashMap::new();
        for &name in SERIES_NAMES {
            series.insert(name, Series::new(maxlen));
            derived.insert(name, 0.0);
            avg.insert(name, [0.0; 3]);
        }
        Self {
            maxlen,
            series,
            derived,
            avg,
            prev: None,
            avg_seeded: false,
            avg_t: None,
        }
    }

    pub fn update(&mut self, snap: Snapshot) {
        let prev = self.prev.take();
        let v = &snap.backend;

        let (
            gen_rate,
            prompt,
            ttft,
            tpot,
            e2e,
            queue,
            req_prompt,
            req_gen,
            req_prefill,
            req_decode,
        ) = match &prev {
            Some(p) if p.backend.reachable && v.reachable => {
                let (pt, ct) = (p.monotonic, snap.monotonic);
                let gen_rate = compute_rate(
                    p.backend.generation_tokens_total,
                    pt,
                    v.generation_tokens_total,
                    ct,
                );
                let prompt =
                    compute_rate(p.backend.prompt_tokens_total, pt, v.prompt_tokens_total, ct);
                let ttft = histogram_recent_avg(&p.backend.ttft, &v.ttft);
                let tpot = histogram_recent_avg(&p.backend.inter_token, &v.inter_token);
                let e2e = histogram_recent_avg(&p.backend.e2e, &v.e2e);
                let queue = histogram_recent_avg(&p.backend.queue_time, &v.queue_time);
                let req_prompt = histogram_avg(&p.backend.req_prompt_tokens, &v.req_prompt_tokens);
                let req_gen = histogram_avg(&p.backend.req_gen_tokens, &v.req_gen_tokens);
                let req_prefill = histogram_avg(&p.backend.prefill_time, &v.prefill_time);
                let req_decode = histogram_avg(&p.backend.decode_time, &v.decode_time);
                (
                    gen_rate,
                    prompt,
                    ttft,
                    tpot,
                    e2e,
                    queue,
                    req_prompt,
                    req_gen,
                    req_prefill,
                    req_decode,
                )
            }
            _ => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
        };

        self.set("gen_tok_s", gen_rate);
        self.set("prompt_tok_s", prompt);
        self.set("ttft", ttft);
        self.set("tpot", tpot);
        self.set("e2e", e2e);
        self.set("queue_time", queue);
        self.set("req_prompt_tok", req_prompt);
        self.set("req_gen_tok", req_gen);
        self.set("req_prefill", req_prefill);
        self.set("req_decode", req_decode);

        // vLLM gauges (instantaneous)
        self.set("running", v.num_requests_running);
        self.set("waiting", v.num_requests_waiting);
        self.set("kv_cache", v.kv_cache_usage_perc * 100.0);

        self.update_windows(snap.monotonic);
        self.prev = Some(snap);
    }

    /// Reset all series, derived values, EMA windows, and the previous-snapshot
    /// baseline. Called when the backend kind changes (vLLM ↔ SGLang) so stale
    /// series from the old metric names don't pollute charts.
    pub fn clear(&mut self) {
        *self = Self::new(self.maxlen);
    }

    fn set(&mut self, name: &'static str, value: f64) {
        if let Some(s) = self.series.get_mut(name) {
            s.append(value);
        }
        if let Some(d) = self.derived.get_mut(name) {
            *d = value;
        }
    }

    /// Fold the latest derived values into the 1/5/15-minute EMAs.
    ///
    /// The first sample seeds every window to the current value (so gauges
    /// don't spend 15 minutes ramping from zero); subsequent samples decay
    /// toward the new value with `alpha = 1 - exp(-dt/tau)`, which keeps the
    /// averaging correct under a variable poll interval.
    fn update_windows(&mut self, now: f64) {
        if !self.avg_seeded {
            for &name in SERIES_NAMES {
                let val = *self.derived.get(name).unwrap_or(&0.0);
                self.avg.insert(name, [val; 3]);
            }
            self.avg_seeded = true;
            self.avg_t = Some(now);
            return;
        }
        let prev_t = self.avg_t.unwrap_or(now);
        let dt = now - prev_t;
        self.avg_t = Some(now);
        if dt <= 0.0 {
            return;
        }
        let alphas: [f64; 3] = WINDOW_TAUS.map(|tau| 1.0 - (-dt / tau).exp());
        for &name in SERIES_NAMES {
            let val = *self.derived.get(name).unwrap_or(&0.0);
            if let Some(emas) = self.avg.get_mut(name) {
                for (i, a) in alphas.iter().enumerate() {
                    emas[i] += a * (val - emas[i]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HISTORY_LEN;
    use crate::ui::widgets::{braille_chart, fmt_duration, stacked_chart_down};

    #[test]
    fn braille_chart_flip() {
        let kw = (1, 1, 0.0, 1.0, false);
        let normal = braille_chart(&[0.25], kw.0, kw.1, kw.2, kw.3, kw.4, false);
        let flipped = braille_chart(&[0.25], kw.0, kw.1, kw.2, kw.3, kw.4, true);
        assert_ne!(normal, flipped);
        let n = normal[0].chars().next().unwrap() as u32;
        let f = flipped[0].chars().next().unwrap() as u32;
        assert_eq!(n - 0x2800, 0x80);
        assert_eq!(f - 0x2800, 0x08);
    }

    #[test]
    fn stacked_chart_down_bands() {
        let g = stacked_chart_down(&[1.0], &[0.0], 1, 1, 1.0, false);
        assert_eq!(g[0][0].1, 0);
        let g = stacked_chart_down(&[0.0], &[1.0], 1, 1, 1.0, false);
        assert_eq!(g[0][0].1, 1);
        let g = stacked_chart_down(&[], &[], 1, 1, 1.0, false);
        assert_eq!(g[0][0], (' ', -1));
    }

    #[test]
    fn fmt_duration_values() {
        assert_eq!(fmt_duration(45.0), "45s");
        assert_eq!(fmt_duration(12.0 * 60.0), "12m");
        assert_eq!(fmt_duration(3.0 * 3600.0 + 20.0 * 60.0), "3h 20m");
        assert_eq!(fmt_duration(2.0 * 86400.0 + 4.0 * 3600.0), "2d 4h");
        assert_eq!(fmt_duration(-1.0), "—");
        assert_eq!(fmt_duration(f64::INFINITY), "—");
    }

    #[test]
    fn compute_rate_basic() {
        assert_eq!(compute_rate(100.0, 10.0, 200.0, 12.0), 50.0);
    }

    #[test]
    fn compute_rate_guards() {
        assert_eq!(compute_rate(100.0, 10.0, 200.0, 10.0), 0.0);
        assert_eq!(compute_rate(200.0, 10.0, 50.0, 12.0), 0.0);
    }

    #[test]
    fn histogram_recent_avg_values() {
        let prev = Histogram {
            count: 10.0,
            sum: 5.0,
            ..Histogram::new()
        };
        let cur = Histogram {
            count: 14.0,
            sum: 13.0,
            ..Histogram::new()
        };
        assert_eq!(histogram_recent_avg(&prev, &cur), 2.0);
        assert_eq!(histogram_recent_avg(&cur, &cur), 0.0);
    }

    #[test]
    fn histogram_quantile_value() {
        let prev = Histogram {
            count: 0.0,
            buckets: vec![(1.0, 0.0), (2.0, 0.0), (5.0, 0.0), (f64::INFINITY, 0.0)],
            ..Histogram::new()
        };
        let cur = Histogram {
            count: 10.0,
            buckets: vec![(1.0, 0.0), (2.0, 0.0), (5.0, 10.0), (f64::INFINITY, 10.0)],
            ..Histogram::new()
        };
        let q = histogram_quantile(&prev, &cur, 0.5);
        assert!((2.0..=5.0).contains(&q));
    }

    #[test]
    fn series_ring_buffer() {
        let mut s = Series::new(3);
        for i in 0..5 {
            s.append(i as f64);
        }
        assert_eq!(s.values(), vec![2.0, 3.0, 4.0]);
        assert_eq!(s.last(), 4.0);
    }

    fn snap(t: f64, gen_total: f64, running: f64) -> Snapshot {
        Snapshot::new(
            t,
            BackendSnapshot {
                reachable: true,
                generation_tokens_total: gen_total,
                num_requests_running: running,
                ..BackendSnapshot::default()
            },
        )
    }

    #[test]
    fn history_rate_from_two_samples() {
        let mut h = History::new(HISTORY_LEN);
        h.update(snap(0.0, 1000.0, 1.0));
        h.update(snap(2.0, 1200.0, 2.0));
        assert_eq!(*h.derived.get("gen_tok_s").unwrap(), 100.0);
        assert_eq!(*h.derived.get("running").unwrap(), 2.0);
        assert_eq!(h.series["gen_tok_s"].values(), vec![0.0, 100.0]);
    }

    #[test]
    fn history_clear_resets_all() {
        let mut h = History::new(HISTORY_LEN);
        h.update(snap(0.0, 1000.0, 1.0));
        h.update(snap(2.0, 1200.0, 2.0));
        assert!(!h.series["gen_tok_s"].is_empty());

        h.clear();

        for name in SERIES_NAMES {
            assert!(h.series[name].is_empty(), "{name} not cleared");
            assert_eq!(
                *h.derived.get(name).unwrap(),
                0.0,
                "{name} derived not reset"
            );
            assert_eq!(*h.avg.get(name).unwrap(), [0.0; 3], "{name} avg not reset");
        }
    }

    #[test]
    fn window_avg_seeds_then_converges() {
        let mut h = History::new(HISTORY_LEN);
        h.update(snap(0.0, 0.0, 1.0));
        assert_eq!(*h.avg.get("running").unwrap(), [1.0, 1.0, 1.0]);
        for i in 1..400 {
            h.update(snap(i as f64, 0.0, 5.0));
        }
        let emas = h.avg["running"];
        assert!(emas[0] > emas[1] && emas[1] > emas[2] && emas[2] > 1.0);
        assert!((emas[0] - 5.0).abs() < 0.5);
    }

    #[test]
    fn braille_chart_shape() {
        let series: Vec<f64> = (0..100).map(|i| ((i as f64) / 5.0).sin()).collect();
        let (w, h) = (20usize, 4usize);
        let rows = braille_chart(&series, w, h, 0.0, 1.0, true, false);
        assert_eq!(rows.len(), h);
        for row in &rows {
            assert_eq!(row.chars().count(), w);
            assert!(row.chars().all(|c| {
                let u = c as u32;
                (0x2800..=0x28FF).contains(&u)
            }));
        }
    }
}
