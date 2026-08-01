//! Headless `--dump-json` path: collect two snapshots an interval apart (so
//! rates are populated), serialise the derived metrics to JSON, and exit —
//! works without a TTY. Without the flag the interactive [`crate::ui::app::App`]
//! runs instead.

use std::thread;
use std::time::Duration;

use clap::builder::{
    Styles,
    styling::{AnsiColor, Effects},
};
use serde_json::json;

use crate::collectors::{self, Backend};
use crate::config::AppConfig;
use crate::state::{History, SERIES_NAMES, Snapshot, monotonic_now};

/// Styled help/error output, matching the look of `cargo`'s own CLI.
pub fn cargo_styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
        .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
        .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .placeholder(AnsiColor::Cyan.on_default())
        .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
        .valid(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
        .invalid(AnsiColor::Yellow.on_default().effects(Effects::BOLD))
}

/// Take two snapshots `interval` apart so rates are populated.
pub fn collect_pair(config: &AppConfig) -> (Snapshot, Snapshot) {
    let collector: Box<dyn Backend> =
        collectors::make_collector(config.backend, config.metrics_url(), config.http_timeout);

    let once = || Snapshot::new(monotonic_now(), collector.poll());

    let first = once();
    thread::sleep(Duration::from_secs_f64(config.interval.min(2.0)));
    let second = once();
    (first, second)
}

/// Collect two snapshots, derive rates, print as JSON, exit. No TTY needed.
pub fn dump_json(config: &AppConfig) -> std::process::ExitCode {
    let (first, second) = collect_pair(config);
    let mut history = History::new(config.history_len);
    history.update(first);
    // `update` takes the snapshot by value; keep `second` for the raw fields below.
    history.update(second.clone());

    let mut derived = serde_json::Map::new();
    for &name in SERIES_NAMES {
        let v = *history.derived.get(name).unwrap_or(&0.0);
        derived.insert(name.to_string(), json!(v));
    }

    let out = json!({
        "url": config.url,
        "reachable": second.backend.reachable,
        "error": second.backend.error,
        "model_name": second.backend.model_name,
        "derived": derived,
        "vllm_info": {
            "process_start_time": second.backend.process_start_time,
            "cache_dtype": second.backend.cache_dtype,
            "block_size": second.backend.block_size,
            "gpu_memory_utilization": second.backend.gpu_memory_utilization,
            "num_gpu_blocks": second.backend.num_gpu_blocks,
            "enable_prefix_caching": second.backend.enable_prefix_caching,
            "engine_awake": second.backend.engine_awake,
            "request_success_total": second.backend.request_success_total,
            "backend": config.backend.as_str(),
        },
        "raw_vllm": {
            "num_requests_running": second.backend.num_requests_running,
            "num_requests_waiting": second.backend.num_requests_waiting,
            "kv_cache_usage_perc": second.backend.kv_cache_usage_perc,
            "generation_tokens_total": second.backend.generation_tokens_total,
            "prompt_tokens_total": second.backend.prompt_tokens_total,
            "num_preemptions_total": second.backend.num_preemptions_total,
            "prefix_cache_hit_rate": second.backend.prefix_cache_hit_rate(),
        },
    });

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    std::process::ExitCode::SUCCESS
}
