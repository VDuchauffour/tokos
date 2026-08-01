# tokos

> _τόκος_ — Greek for "offspring"; the root of "interest"\`

A lightweight terminal UI for real-time monitoring of inference server metrics.

## Features

- **CLI parsing** with [`clap`](https://github.com/clap-rs/clap)
- **Error handling** with [`anyhow`](https://github.com/dtijv/anyhow)
- **Dev container** with Rust, `just`, `cargo-tarpaulin`, and `pre-commit`
- **CI/CD** via GitHub Actions (format, lint, test, coverage, draft releases)
- **Task runner** via [`just`](https://github.com/casey/just)
- **Pre-commit hooks** for formatting and linting
- **Renovate** config for automated dependency updates

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

`mock-server` flags mirror [guidellm](https://github.com/vllm-project/guidellm):
`--host`, `--port`, `--model`, `--request-latency`, `--request-latency-std`,
`--ttft-ms`, `--ttft-ms-std`, `--itl-ms`, `--itl-ms-std`, `--output-tokens`,
`--output-tokens-std`, and `--auto-traffic`. Run `tokos mock-server --help` for
the full list.

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
