# tokos

> _τόκος_ — Greek for "offspring"; the root of "interest"\`

A lightweight terminal UI for real-time monitoring of inference server metrics.

Supports [vLLM](https://github.com/vllm-project/vllm) and [SGLang](https://github.com/sgl-project/sglang) via their Prometheus `/metrics` endpoints.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/VDuchauffour/tokos/main/install.sh | bash
```

You can also install with `cargo install tokos`

## Features

- **Real-time TUI** — btop-style dashboard with braille charts
- **Prometheus scraping** — rates, histogram quantiles (TTFT, ITL, e2e/queue latency), prefix-cache hit ratios
- **Live request feed** — tail a log file or `docker logs -f`
- **Headless JSON** — `--dump-json` for scripting and CI
- **Mock server** — `mock-server --generate-traffic` for testing without a deployment

## Usage

`tokos` has three subcommands: `run` launches the TUI against a live
`/metrics` endpoint, `mock-server` starts a synthetic server for testing
the TUI without a real deployment, and `completions` prints shell completion
scripts. A subcommand is always required.

```sh
# Launch the TUI
tokos run --url http://localhost:8000

# Headless: collect two snapshots, print derived metrics as JSON, exit
tokos run --url http://localhost:8000 --dump-json

# Start a mock vLLM server (serves /metrics, /v1/models, /v1/chat/completions)
tokos mock-server --backend vllm --port 8000 --model GLM-5.2

# Start a mock SGLang server
tokos mock-server --backend sglang --port 30000 --generate-traffic
# in another terminal:
tokos run --url http://127.0.0.1:30000

# Generate shell completions (auto-detects from $SHELL when no shell is given)
tokos completions >/etc/bash_completion.d/tokos
tokos completions zsh >~/.zfunc/_tokos
tokos completions fish >~/.config/fish/completions/tokos.fish
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
| `--backend`             | `TOKOS_MOCK_BACKEND`             | —           | `vllm` or `sglang` (required, no default)  |
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
| `--generate-traffic`    | `TOKOS_MOCK_GENERATE_TRAFFIC`    | `false`     | spawn a background thread driving requests |
| `--no-color`            | `TOKOS_MOCK_NO_COLOR`            | `false`     | disable colored log output                 |

## Usage

```sh
tokos run --url http://localhost:8000 # auto-detect backend (default)
tokos run --url http://localhost:30000 --backend sglang
tokos run --url http://localhost:8000 --backend vllm
```

| Flag          | Env              | Default                 | Description                                                         |
| ------------- | ---------------- | ----------------------- | ------------------------------------------------------------------- |
| `--url`       | `TOKOS_URL`      | `http://localhost:8000` | Inference server base URL                                           |
| `--backend`   | `TOKOS_BACKEND`  | `auto`                  | `auto`, `vllm`, or `sglang`                                         |
| `--interval`  | —                | `1.0`                   | Poll interval in seconds                                            |
| `--log-file`  | `TOKOS_LOG_FILE` | —                       | Tail a log file for the request feed (vLLM `--enable-log-requests`) |
| `--docker`    | `TOKOS_DOCKER`   | —                       | Stream `docker logs -f` from a container                            |
| `--dump-json` | —                | —                       | Collect two snapshots, print derived metrics as JSON, exit          |

With `--backend auto` (the default), tokos probes `/metrics` once and sniffs the metric-name prefix (`vllm:` vs `sglang:`) to pick the right parser. An explicit `--backend vllm|sglang` skips the probe.

## Development

### Getting Started

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

3. Publish the drafted release on GitHub to trigger the [release](.github/workflows/release.yml) workflow, which publishes the crate to crates.io and uploads prebuilt binaries to the release.
