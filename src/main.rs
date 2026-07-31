//! `tokos` — a btop-style terminal UI for monitoring a vLLM instance in
//! real time, built on ratatui.

// Many helpers (formatters, Series accessors, histogram_quantile, etc.) are
//! kept for unit tests even though
// the binary entry point doesn't call them directly.
#![allow(dead_code)]

use std::process::ExitCode;

use clap::Parser;

mod cli;
mod collectors;
mod config;
mod state;
mod ui;

use config::{DEFAULT_INTERVAL, DEFAULT_URL};

/// A btop-style TUI for monitoring a vLLM instance.
#[derive(Parser)]
#[command(name = "tokos", version, about, styles = cli::cargo_styles())]
struct Args {
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

fn build_config(args: Args) -> config::AppConfig {
    config::AppConfig {
        url: args.url,
        interval: args.interval.max(0.1),
        history_len: config::HISTORY_LEN,
        http_timeout: config::HTTP_TIMEOUT,
        log_file: args.log_file,
        docker_container: args.docker,
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
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
