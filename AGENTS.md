# AGENTS.md

Compact guidance for OpenCode sessions working in `tokos`. Read this before
editing — it covers the non-obvious toolchain and testing quirks.

## What this is

Rust TUI (`ratatui` + `crossterm`) that scrapes vLLM / SGLang Prometheus
`/metrics` endpoints. Single binary crate, no workspace, Rust edition 2024.
`node_modules/` exists only for the prettier pre-commit hook — this is not a
Node project.

## Build & verify

All dev tasks go through `just` (see `justfile`). Non-obvious points:

- **`cargo +nightly fmt` is required.** `rustfmt.toml` sets
  `group_imports = "StdExternalCrate"`, an unstable option. CI installs a
  nightly toolchain specifically for `just fmt-check`. Use `just fmt` /
  `just fmt-check`, never plain `cargo fmt`.
- **`just lint-strict`** = `cargo clippy --all-targets --all-features -- -D warnings`.
  Warnings fail CI; fix them or run `just lint-strict-fix` to auto-apply.
- **CI runs `just fmt-check` + `just lint-strict` only — it does NOT run
  `just test`.** Tests run only in the coverage job (via `cargo-tarpaulin`).
  Run `just test` locally before pushing.
- **Full local gate:** `just ci` (=`fmt-check + lint-strict + test`).
  Auto-fix variant: `just ci-fix` (=`fmt + lint-strict-fix + test`).
- rust-analyzer uses a separate `CARGO_TARGET_DIR=target/rust-analyzer` (set in
  `.vscode/` and `.devcontainer/`) so it doesn't lock the main `target/`. Don't
  "fix" this by removing the override.

## Testing conventions

- **There are no integration test files in `tests/`.** That directory holds
  only Prometheus exposition-text fixtures (`metrics_fixture.txt`,
  `sglang_metrics_fixture.txt`). All unit tests are inline `#[cfg(test)] mod tests` blocks inside `src/**/*.rs`.
- Fixtures are loaded with `include_str!("../../tests/<fixture>.txt")` from
  the collector test modules. Add new fixtures there, not as standalone test
  binaries.
- Run a single test by name: `cargo test <test_name>` (not `--test`, since
  there are no `tests/*.rs` files).
- `src/main.rs` carries `#![allow(dead_code)]` deliberately: many helpers
  (formatters, `histogram_quantile`, `Series` accessors) exist only for unit
  tests. Do not "clean up" apparently-unused public items without checking
  test modules first.

## Architecture

Entry flow (`src/main.rs`): clap-derive `Cli` with two subcommands — `run`
(the default) and `mock-server`. `run`'s flags are flattened to the top level
via `args_conflicts_with_subcommands`, so `tokos --url X` is shorthand for
`tokos run --url X` and bare `tokos` launches the TUI. `run` dispatches to
either headless `cli::dump_json` (collect two snapshots, print JSON, exit, no
TTY needed) or `ui::app::App::run` (TUI render loop). `mock-server` calls
`mock_server::run`.

Module ownership:

- `src/config.rs` — `AppConfig`, the single config struct passed to every
  collector and the UI. Defaults live as `pub const` here.
- `src/cli.rs` — headless `--dump-json` path: `collect_pair()` (two snapshots
  `interval` apart) and `dump_json()` (serialize derived metrics, print, exit,
  no TTY). Also `cargo_styles()` for clap help styling.
- `src/logging.rs` — tracing/tracing-subscriber init driven by `run`'s
  `--log-level` / `--trace-file` / `--log-format` (text|json) flags; single
  sink (file or stderr), respects `RUST_LOG` via `EnvFilter`.
- `src/collectors/` — `Backend` trait + impls: `vllm`, `sglang`,
  `access_log` (tails a log file or `docker logs -f`), `common` (shared
  exposition-text parser + HTTP fetcher). `run`'s `--backend` flag
  (`auto`/`vllm`/`sglang`, env `TOKOS_BACKEND`; default `auto`) selects the
  collector. `AutoCollector` sniffs the `vllm:` / `sglang:` metric-name prefix
  to pick the parser (falls back to vllm) and **re-probes every 30 polls**
  (`REPROBE_EVERY`) to catch mid-session server swaps; the trait's
  `effective_kind()` exposes the detected kind so the UI clears `History` on
  a swap.
- `src/mock_server.rs` — std-only mock vLLM **or SGLang** server
  (`mock-server` subcommand; `--backend vllm|sglang`, no `auto`). Serves
  `/metrics` (Prometheus exposition text that round-trips through the matching
  `collectors::{vllm,sglang}::parse_metrics`), `/health`, `/v1/models`, and
  `/v1/{chat,}completions`. Simulated requests update counters/histograms;
  `--generate-traffic` drives them from a background thread. No new deps (no
  `tokio`/`axum`/`rand` — thread-per-connection `std::net` + a xorshift RNG).
- `src/state.rs` — the most central file for metric derivation.
  `BackendSnapshot` (raw scrape), `History` (ring-buffer series + 1/5/15-min
  EMA windows), and all rate/quantile math (`compute_rate`,
  `histogram_quantile`, `histogram_recent_avg`). Changes to metric derivation
  belong here, not in collectors or UI.
- `src/ui/` — ratatui app split into `app` (poller thread + render loop),
  `layout`, `panels`, `registry`, `theme`, `views`, `widgets`.

## Pre-commit hooks

`just pre-commit-install` runs `uvx pre-commit install` — **requires `uv`**
(hooks execute via `uvx pre-commit`). Hooks: `pre-commit-hooks` (trailing
whitespace, end-of-file-fixer, `check-{xml,json,yaml,toml}`, debug-statements,
check-executables-have-shebangs, check-case-conflict, check-added-large-files,
detect-private-key), `yamlfix`, `taplo` (toml), `prettier` (json only),
`mdformat` (markdown), and a local `just check` hook. The devcontainer
provisions pre-commit via `pipx`/`pip` (not `uvx`) in
`.devcontainer/post-create.sh` — so `uv` is only needed for the `just` recipe,
not inside the devcontainer.

## Release & PR conventions

- **PR titles must follow Conventional Commits** (enforced by
  `amannn/action-semantic-pull-request`): lowercase subject
  (`^(?![A-Z]).+$`), no scope required, single-commit PRs enforced
  (`validateSingleCommit`). PRs labeled `dependencies` bypass title
  validation.
- PR labeling runs in `.github/workflows/pr-enhancement.yml` via two jobs:
  branch-based (`TimonVS/pr-labeler-action`, config in
  `.github/pr-labeler.yml`) maps `feature/*`/`feat/*`, `fix/*`/`fixes/*`,
  `chore/*`, `renovate/*`/`update/*`/`deps/*`, etc. to labels; and
  path/label-based (`release-drafter`). Labels then map to release-drafter
  categories (Features, Bug Fixes, Maintenance, Documentation, Dependencies)
  in `.github/release-drafter.yml`, which also resolves the next version from
  `major`/`minor`/`patch` labels.
- An opencode review bot (`.github/workflows/opencode.yml`) runs on PR/issue
  comments containing `/oc` or `/opencode`.
- Release flow: push annotated tag `vX.Y.Z` → `release-drafter` drafts notes
  on every push to `main` → publish the GitHub release → `publish.yml`
  builds and runs `cargo publish` to crates.io. Do not `cargo publish`
  manually.
- Renovate automerges minor/patch for cargo, github-actions, and pre-commit;
  major bumps require manual review.
