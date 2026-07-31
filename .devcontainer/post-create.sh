#!/usr/bin/env bash
# Provision the development environment to match the project's CI pipeline.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

echo "==> Adding rustup components (clippy, rustfmt)"
rustup component add clippy rustfmt

echo "==> Installing nightly toolchain (required for 'cargo +nightly fmt')"
rustup toolchain install nightly --profile minimal --component rustfmt

echo "==> Installing cargo-binstall (for fast prebuilt binary installs)"
if ! command -v cargo-binstall >/dev/null 2>&1; then
	curl -L --proto '=https' --tlsv1.2 -sSf \
		https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
fi

echo "==> Installing dev tools (just, cargo-tarpaulin)"
cargo binstall -y just cargo-tarpaulin

echo "==> Installing pre-commit"
if command -v pipx >/dev/null 2>&1; then
	pipx install pre-commit
else
	pip install --user pre-commit
fi

echo "==> Installing git hooks"
if command -v pre-commit >/dev/null 2>&1; then
	pre-commit install
fi

echo "==> Setup complete. Try: just --list"
