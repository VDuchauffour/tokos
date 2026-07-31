# tokos

> _τόκος_ — Greek for "offspring"; the root of "interest"`

A lightweight terminal UI for real-time monitoring of inference server metrics.

## Features

- **CLI parsing** with [`clap`](https://github.com/clap-rs/clap)
- **Error handling** with [`anyhow`](https://github.com/dtijv/anyhow)
- **Dev container** with Rust, `just`, `cargo-tarpaulin`, and `pre-commit`
- **CI/CD** via GitHub Actions (format, lint, test, coverage, draft releases)
- **Task runner** via [`just`](https://github.com/casey/just)
- **Pre-commit hooks** for formatting and linting
- **Renovate** config for automated dependency updates

## Getting Started

### Development

To ensure that you follow the development workflow, please setup the pre-commit hooks:

```sh
just pre-commit-install
```

> **Note:** This requires [`uv`](https://github.com/astral-sh/uv) to be installed, as the hooks are run via `uvx pre-commit`.

Common tasks:

```sh
just        # list all recipes
just run    # cargo run
just test   # cargo test
just ci     # fmt-check + lint-strict + test
```

### Release

1. Push a tag: `git tag -a v0.1.0 -m "Release v0.1.0" && git push origin v0.1.0`
2. The [release-drafter](.github/workflows/release-drafter.yml) workflow auto-drafts release notes on every push to `main`.

3. Publish the drafted release on GitHub to trigger the [publish](.github/workflows/publish.yml) workflow, which publishes the crate to crates.io.
