default:
    @just --list

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all-features

nextest:
    cargo nextest run --all-features

check:
    cargo check --all-features

build:
    cargo build --all-features

build-release:
    cargo build --release --all-features

doc:
    cargo doc --all-features --no-deps

doc-open:
    cargo doc --all-features --no-deps --open

deny:
    cargo deny check

deny-licenses:
    cargo deny check licenses

deny-bans:
    cargo deny check bans

deny-sources:
    cargo deny check sources

audit:
    cargo audit

ci: fmt-check clippy test deny

pre-commit: fmt clippy

clean:
    cargo clean

update:
    cargo update

tree:
    cargo tree

outdated:
    cargo outdated -R

run *ARGS:
    cargo run --release -- {{ARGS}}

watch:
    cargo watch -x check

watch-test:
    cargo watch -x test
