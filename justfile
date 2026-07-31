_default:
    @just --list

pre-commit-install:
    uvx pre-commit install

install:
    cargo install --path .

clean:
    cargo clean

update:
    cargo update

build:
    cargo build

release:
    cargo build --release

run *args:
    cargo run -- {{args}}

run-release *args:
    cargo run --release -- {{args}}

test:
    cargo test

lint:
    cargo clippy --all-targets --all-features

lint-strict:
    cargo clippy --all-targets --all-features -- -D warnings

lint-strict-fix:
    cargo clippy --fix --allow-dirty --all-targets --all-features -- -D warnings

fmt:
    cargo +nightly fmt

fmt-check:
    cargo +nightly fmt --check

check:
    cargo check

doc:
    cargo doc --no-deps --open

ci: fmt-check lint-strict test

ci-fix: fmt lint-strict-fix test
