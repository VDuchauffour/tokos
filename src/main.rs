//! `tokos` — a btop-style terminal UI for monitoring a vLLM instance in
//! real time, built on ratatui.

// Many helpers (formatters, Series accessors, histogram_quantile, etc.) are
// kept for unit tests even though
// the binary entry point doesn't call them directly.
#![allow(dead_code)]

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::config::{DEFAULT_INTERVAL, DEFAULT_URL};
use crate::logging::LogFormat;
use crate::mock_server::DEFAULT_MODEL;

mod cli;
mod collectors;
mod config;
mod logging;
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

    /// structured log level: error, warn, info, debug, trace, off
    /// (env TOKOS_LOG_LEVEL; default: off in TUI, warn in --dump-json)
    #[arg(long, env = "TOKOS_LOG_LEVEL")]
    log_level: Option<String>,

    /// write structured logs to this file (use in TUI mode to avoid corrupting
    /// the terminal; env TOKOS_TRACE_FILE)
    #[arg(long, env = "TOKOS_TRACE_FILE")]
    trace_file: Option<String>,

    /// structured log output format: text or json
    /// (env TOKOS_LOG_FORMAT; default: json when --trace-file is set, text otherwise)
    #[arg(long, env = "TOKOS_LOG_FORMAT", value_enum)]
    log_format: Option<LogFormat>,
}

/// Arguments for the `mock-server` subcommand. Mirror guidellm's flags.
///
/// Every field also reads from a `TOKOS_MOCK_*` environment variable, so the
/// mock server can be configured without touching the CLI — useful for
/// containers and test harnesses. CLI flags take precedence over env vars,
/// which take precedence over the defaults.
#[derive(Parser, Clone)]
struct MockServerArgs {
    /// host address to bind the server to
    /// (env TOKOS_MOCK_HOST; default: 127.0.0.1)
    #[arg(long, env = "TOKOS_MOCK_HOST", default_value = "127.0.0.1")]
    host: String,

    /// port to bind the server to
    /// (env TOKOS_MOCK_PORT; default: 8000)
    #[arg(long, env = "TOKOS_MOCK_PORT", default_value_t = 8000)]
    port: u16,

    /// name of the model to mock
    /// (env TOKOS_MOCK_MODEL; default: GLM-5.2)
    #[arg(long, env = "TOKOS_MOCK_MODEL", default_value = DEFAULT_MODEL)]
    model: String,

    /// base request latency in seconds for non-streaming responses
    /// (env TOKOS_MOCK_REQUEST_LATENCY; default: 3.0)
    #[arg(long, env = "TOKOS_MOCK_REQUEST_LATENCY", default_value_t = 3.0)]
    request_latency: f64,

    /// standard deviation for request latency
    /// (env TOKOS_MOCK_REQUEST_LATENCY_STD; default: 0.0)
    #[arg(long, env = "TOKOS_MOCK_REQUEST_LATENCY_STD", default_value_t = 0.0)]
    request_latency_std: f64,

    /// time to first token in milliseconds for streaming responses
    /// (env TOKOS_MOCK_TTFT_MS; default: 150.0)
    #[arg(long, env = "TOKOS_MOCK_TTFT_MS", default_value_t = 150.0)]
    ttft_ms: f64,

    /// standard deviation for time to first token
    /// (env TOKOS_MOCK_TTFT_MS_STD; default: 0.0)
    #[arg(long, env = "TOKOS_MOCK_TTFT_MS_STD", default_value_t = 0.0)]
    ttft_ms_std: f64,

    /// inter-token latency in milliseconds for streaming responses
    /// (env TOKOS_MOCK_ITL_MS; default: 10.0)
    #[arg(long, env = "TOKOS_MOCK_ITL_MS", default_value_t = 10.0)]
    itl_ms: f64,

    /// standard deviation for inter-token latency
    /// (env TOKOS_MOCK_ITL_MS_STD; default: 0.0)
    #[arg(long, env = "TOKOS_MOCK_ITL_MS_STD", default_value_t = 0.0)]
    itl_ms_std: f64,

    /// number of output tokens to generate per request
    /// (env TOKOS_MOCK_OUTPUT_TOKENS; default: 128)
    #[arg(long, env = "TOKOS_MOCK_OUTPUT_TOKENS", default_value_t = 128)]
    output_tokens: u32,

    /// standard deviation for output token count
    /// (env TOKOS_MOCK_OUTPUT_TOKENS_STD; default: 0.0)
    #[arg(long, env = "TOKOS_MOCK_OUTPUT_TOKENS_STD", default_value_t = 0.0)]
    output_tokens_std: f64,

    /// spawn a background thread that generates requests so metrics move
    /// without external traffic
    /// (env TOKOS_MOCK_GENERATE_TRAFFIC; default: false)
    #[arg(long, env = "TOKOS_MOCK_GENERATE_TRAFFIC")]
    generate_traffic: bool,

    /// disable colored log output
    /// (env TOKOS_MOCK_NO_COLOR; default: false)
    #[arg(long, env = "TOKOS_MOCK_NO_COLOR")]
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
            generate_traffic: a.generate_traffic,
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
    let log_level = args.log_level.clone();
    let trace_file = args.trace_file.clone();
    let log_format = args.log_format;
    let config = build_config(args);

    // Default: warn in headless mode, off in TUI mode (stderr corrupts the
    // alt-screen; use --trace-file to capture TUI logs to a file).
    let level = log_level
        .as_deref()
        .unwrap_or(if dump { "warn" } else { "off" });
    // Default to JSON when logging to a file (machine-parseable), text when
    // going to stderr (human-readable).
    let format = log_format.unwrap_or(if trace_file.is_some() {
        LogFormat::Json
    } else {
        LogFormat::Text
    });
    logging::init(level, trace_file.as_deref(), true, format);

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
