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

Entry flow (`src/main.rs`): clap-derive `Args` → either headless
`cli::dump_json` (collect two snapshots, print JSON, exit, no TTY needed) or
`ui::app::App::run` (TUI render loop).

Module ownership:

- `src/config.rs` — `AppConfig`, the single config struct passed to every
  collector and the UI. Defaults live as `pub const` here.
- `src/collectors/` — `Backend` trait + impls: `vllm`, `sglang`,
  `access_log` (tails a log file or `docker logs -f`), `common` (shared
  exposition-text parser + HTTP fetcher). `AutoCollector` probes `/metrics`
  once and sniffs the `vllm:` / `sglang:` metric-name prefix to pick the
  parser; falls back to vllm.
- `src/state.rs` — the largest and most central file. `BackendSnapshot` (raw
  scrape), `History` (ring-buffer series + 1/5/15-min EMA windows), and all
  rate/quantile math (`compute_rate`, `histogram_quantile`,
  `histogram_recent_avg`). Changes to metric derivation belong here, not in
  collectors or UI.
- `src/ui/` — ratatui app split into `app` (poller thread + render loop),
  `layout`, `panels`, `registry`, `theme`, `views`, `widgets`.

## Pre-commit hooks

`just pre-commit-install` runs `uvx pre-commit install` — **requires `uv` to
be installed** (hooks execute via `uvx pre-commit`). Hooks cover: trailing
whitespace, yaml/yamlfix, toml/taplo, json/prettier, markdown/mdformat, and a
local `just check` hook. The devcontainer provisions all of this
automatically (`.devcontainer/post-create.sh`).

## Release & PR conventions

- **PR titles must follow Conventional Commits** (enforced by
  `amannn/action-semantic-pull-request`): lowercase subject
  (`^(?![A-Z]).+$`), no scope required. PRs labeled `dependencies` bypass
  title validation.
- Branch-based auto-labeling (`pr-labeler.yml`): `feature/*`, `fix/*`,
  `chore/*`, `renovate/*`, etc. map to release-drafter categories.
- Release flow: push annotated tag `vX.Y.Z` → `release-drafter` drafts notes
  on every push to `main` → publish the GitHub release → `publish.yml`
  builds and runs `cargo publish` to crates.io. Do not `cargo publish`
  manually.
- Renovate automerges minor/patch for cargo, github-actions, and pre-commit;
  major bumps require manual review.
