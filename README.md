# tokos

> _τόκος_ — Greek for "offspring"; the root of "interest"\`

A lightweight terminal UI for real-time monitoring of inference server metrics.

## Features

- **Real-time TUI** — btop-style terminal dashboard (`ratatui` + `crossterm`)
  with braille charts and rounded box-drawing panels for live vLLM metrics
- **Prometheus scraping** — polls vLLM `/metrics` exposition text and derives
  rates, histogram quantiles (TTFT, ITL, e2e and queue latency), and prefix-cache
  hit ratios from raw counters and buckets
- **Load-average windows** — Unix-style 1/5/15-minute EMA series over throughput
  and latency, mirroring `uptime`'s load-average semantics
- **Switchable views** — `overview`, `1·5·15`, and `requests` layouts cycled with
  the number keys or `Tab`
- **Live request feed** — tails a vLLM request log file or `docker logs -f` to
  stream per-request entries from `--enable-log-requests` output
- **Headless JSON mode** — `--dump-json` collects two snapshots and prints the
  derived metrics as JSON with no TTY, handy for scripting and CI
- **Built-in mock server** — `mock-server` serves a synthetic vLLM (`/metrics`,
  `/v1/models`, `/v1/chat/completions`) with `--auto-traffic` to exercise the TUI
  without a real deployment

## Usage

`tokos` has two subcommands: `run` (the default) launches the TUI against a
live vLLM `/metrics` endpoint, and `mock-server` starts a synthetic vLLM
server for testing the TUI without a real deployment.

`run` is the default subcommand, so its flags are accepted at the top level —
`tokos --url X` is shorthand for `tokos run --url X`, and bare `tokos` launches
the TUI with environment-derived defaults.

```sh
# Launch the TUI (default subcommand)
tokos
tokos --url http://localhost:8000 --interval 1

# Explicit form
tokos run --url http://localhost:8000 --interval 1

# Headless: collect two snapshots, print derived metrics as JSON, exit
tokos --dump-json
tokos run --url http://localhost:8000 --dump-json

# Start a mock vLLM server (serves /metrics, /v1/models, /v1/chat/completions)
tokos mock-server --port 8000 --model GLM-5.2

# Drive the mock with synthetic traffic so the TUI shows live movement
tokos mock-server --port 8000 --auto-traffic
# in another terminal:
tokos --url http://127.0.0.1:8000
```

Common `run` flags:

| Flag          | Env              | Default                 | Description                            |
| ------------- | ---------------- | ----------------------- | -------------------------------------- |
| `--url`       | `TOKOS_URL`      | `http://localhost:8000` | vLLM base URL                          |
| `--interval`  |                  | `1.0`                   | poll interval in seconds               |
| `--log-file`  | `TOKOS_LOG_FILE` |                         | vLLM request log to tail               |
| `--docker`    | `TOKOS_DOCKER`   |                         | container to `docker logs -f`          |
| `--dump-json` |                  | `false`                 | print derived metrics as JSON and exit |

`mock-server` flags mirror [guidellm](https://github.com/vllm-project/guidellm)
and every flag also reads from a `TOKOS_MOCK_*` env var (CLI flag wins over env,
env wins over default):

| Flag                    | Env                              | Default     | Description                                |
| ----------------------- | -------------------------------- | ----------- | ------------------------------------------ |
| `--host`                | `TOKOS_MOCK_HOST`                | `127.0.0.1` | bind address                               |
| `--port`                | `TOKOS_MOCK_PORT`                | `8000`      | bind port                                  |
| `--model`               | `TOKOS_MOCK_MODEL`               | `GLM-5.2`   | model name to advertise                    |
| `--request-latency`     | `TOKOS_MOCK_REQUEST_LATENCY`     | `3.0`       | base request latency in seconds            |
| `--request-latency-std` | `TOKOS_MOCK_REQUEST_LATENCY_STD` | `0.0`       | stddev for request latency                 |
| `--ttft-ms`             | `TOKOS_MOCK_TTFT_MS`             | `150.0`     | time to first token in ms                  |
| `--ttft-ms-std`         | `TOKOS_MOCK_TTFT_MS_STD`         | `0.0`       | stddev for TTFT                            |
| `--itl-ms`              | `TOKOS_MOCK_ITL_MS`              | `10.0`      | inter-token latency in ms                  |
| `--itl-ms-std`          | `TOKOS_MOCK_ITL_MS_STD`          | `0.0`       | stddev for ITL                             |
| `--output-tokens`       | `TOKOS_MOCK_OUTPUT_TOKENS`       | `128`       | output tokens per request                  |
| `--output-tokens-std`   | `TOKOS_MOCK_OUTPUT_TOKENS_STD`   | `0.0`       | stddev for output token count              |
| `--auto-traffic`        | `TOKOS_MOCK_AUTO_TRAFFIC`        | `false`     | spawn a background thread driving requests |
| `--no-color`            | `TOKOS_MOCK_NO_COLOR`            | `false`     | disable colored log output                 |

So `request-latency = 2s` can be set either way:

```sh
tokos mock-server --request-latency 2.0
TOKOS_MOCK_REQUEST_LATENCY=2.0 tokos mock-server
```

## Getting Started

### Development

To ensure that you follow the development workflow, please setup the pre-commit hooks:

```sh
just pre-commit-install
```

> **Note:** This requires [`uv`](https://github.com/astral-sh/uv) to be installed, as the hooks are run via `uvx pre-commit`.

Common tasks:

```sh
just      # list all recipes
just run  # cargo run
just test # cargo test
just ci   # fmt-check + lint-strict + test
```

### Release

1. Push a tag: `git tag -a v0.1.0 -m "Release v0.1.0" && git push origin v0.1.0`

2. The [release-drafter](.github/workflows/release-drafter.yml) workflow auto-drafts release notes on every push to `main`.

3. Publish the drafted release on GitHub to trigger the [publish](.github/workflows/publish.yml) workflow, which publishes the crate to crates.io.
