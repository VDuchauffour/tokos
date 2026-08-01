//! `tokos` — a btop-style terminal UI for monitoring a vLLM instance in
//! real time, built on ratatui.

// Many helpers (formatters, Series accessors, histogram_quantile, etc.) are
// kept for unit tests even though
// the binary entry point doesn't call them directly.
#![allow(dead_code)]

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::config::{DEFAULT_INTERVAL, DEFAULT_URL};

mod cli;
mod collectors;
mod config;
mod mock_server;
mod state;
mod ui;

/// A btop-style TUI for monitoring a vLLM instance.
#[derive(Parser)]
#[command(name = "tokos", version, about, styles = cli::cargo_styles(), args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(flatten)]
    run: RunArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Launch the terminal UI against a live vLLM `/metrics` endpoint.
    Run(RunArgs),

    /// Start a mock vLLM server that serves a synthetic `/metrics` endpoint
    /// for testing `tokos` itself.
    MockServer(MockServerArgs),
}

/// Arguments for the `run` subcommand (the default).
#[derive(Args, Clone)]
struct RunArgs {
    /// vLLM base URL (env TOKOS_URL)
    #[arg(long, env = "TOKOS_URL", default_value = DEFAULT_URL)]
    url: String,

    /// poll interval in seconds (default 1.0)
    #[arg(long, default_value_t = DEFAULT_INTERVAL)]
    interval: f64,

    /// tail this vLLM log file for the requests panel (env TOKOS_LOG_FILE)
    #[arg(long, env = "TOKOS_LOG_FILE")]
    log_file: Option<String>,

    /// stream `docker logs -f` from this container for the requests panel
    /// (env TOKOS_DOCKER)
    #[arg(long, env = "TOKOS_DOCKER")]
    docker: Option<String>,

    /// collect two snapshots, print derived metrics as JSON, and exit (no TTY
    /// needed)
    #[arg(long)]
    dump_json: bool,
}

/// Arguments for the `mock-server` subcommand. Mirror guidellm's flags.
#[derive(Parser, Clone)]
struct MockServerArgs {
    /// host address to bind the server to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// port to bind the server to
    #[arg(long, default_value_t = 8000)]
    port: u16,

    /// name of the model to mock
    #[arg(long, default_value = "llama-3.1-8b-instruct")]
    model: String,

    /// base request latency in seconds for non-streaming responses
    #[arg(long, default_value_t = 3.0)]
    request_latency: f64,

    /// standard deviation for request latency
    #[arg(long, default_value_t = 0.0)]
    request_latency_std: f64,

    /// time to first token in milliseconds for streaming responses
    #[arg(long, default_value_t = 150.0)]
    ttft_ms: f64,

    /// standard deviation for time to first token
    #[arg(long, default_value_t = 0.0)]
    ttft_ms_std: f64,

    /// inter-token latency in milliseconds for streaming responses
    #[arg(long, default_value_t = 10.0)]
    itl_ms: f64,

    /// standard deviation for inter-token latency
    #[arg(long, default_value_t = 0.0)]
    itl_ms_std: f64,

    /// number of output tokens to generate per request
    #[arg(long, default_value_t = 128)]
    output_tokens: u32,

    /// standard deviation for output token count
    #[arg(long, default_value_t = 0.0)]
    output_tokens_std: f64,

    /// spawn a background thread that simulates requests so metrics move
    /// without external traffic
    #[arg(long)]
    auto_traffic: bool,

    /// disable colored log output
    #[arg(long)]
    no_color: bool,
}

impl From<MockServerArgs> for mock_server::MockServerConfig {
    fn from(a: MockServerArgs) -> Self {
        Self {
            host: a.host,
            port: a.port,
            model: a.model,
            request_latency: a.request_latency,
            request_latency_std: a.request_latency_std,
            ttft_ms: a.ttft_ms,
            ttft_ms_std: a.ttft_ms_std,
            itl_ms: a.itl_ms,
            itl_ms_std: a.itl_ms_std,
            output_tokens: a.output_tokens,
            output_tokens_std: a.output_tokens_std,
            auto_traffic: a.auto_traffic,
            no_color: a.no_color,
        }
    }
}

fn build_config(args: RunArgs) -> config::AppConfig {
    config::AppConfig {
        url: args.url,
        interval: args.interval.max(0.1),
        history_len: config::HISTORY_LEN,
        http_timeout: config::HTTP_TIMEOUT,
        log_file: args.log_file,
        docker_container: args.docker,
    }
}

/// Run the default action: TUI or `--dump-json`.
fn run(args: RunArgs) -> ExitCode {
    let dump = args.dump_json;
    let config = build_config(args);

    if dump {
        return cli::dump_json(&config);
    }

    match ui::app::App::new(config).run() {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args = Cli::parse();
    match args.command {
        Some(Command::Run(r)) => run(r),
        Some(Command::MockServer(m)) => mock_server::run(m.into()),
        None => run(args.run),
    }
}
