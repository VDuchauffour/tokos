//! Data collectors: one per source, each returning a snapshot struct.
//!
//! - [`vllm`] — scrape vLLM `/metrics` (Prometheus exposition text)
//! - [`access_log`] — tail a log file or `docker logs` for the request feed
//!   (vLLM request-log lines from `--enable-log-requests`)

pub mod access_log;
pub mod vllm;
