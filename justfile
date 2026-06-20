default:
    @just --list

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

clippy:
    cargo clippy --lib --all-features -- -D warnings
    cargo clippy --tests --benches --all-features -- -D warnings -A clippy::unwrap_used -A clippy::expect_used

test:
    cargo test --all-features

nextest:
    cargo nextest run --all-features

check:
    cargo check --all-features

# Wasm conformance build for the peer stack. Needs a nightly toolchain with
# the wasm32-unknown-unknown target; rustflags come from .cargo/config.toml.
# See docs/agents/wasm.md.
wasm-peers:
    cargo +nightly build --target wasm32-unknown-unknown -p vertex-util-runtime -p vertex-swarm-peer-score -p vertex-swarm-peer-manager -p vertex-swarm-bandwidth-pseudosettle

# Assert the embedded FFI cone stays free of the native observability server
# stack. The server feature of vertex-observability pulls the Prometheus
# exporter and the OTLP appender, which a wasm or embedded client never wants.
# These two crates are the canonical markers: bare `opentelemetry` and `axum`
# still resolve through tracing-opentelemetry and tonic (the gRPC surface), so
# the guard keys off the server-only crates instead.
check-cone:
    #!/usr/bin/env bash
    set -euo pipefail
    tree="$(cargo tree -p vertex-ffi -e normal)"
    leaked=""
    for crate in metrics-exporter-prometheus opentelemetry-appender-tracing; do
        if grep -q "$crate" <<<"$tree"; then
            leaked="$leaked $crate"
        fi
    done
    if [ -n "$leaked" ]; then
        echo "cone guard: vertex-ffi pulls the observability server stack:$leaked" >&2
        exit 1
    fi
    echo "cone guard: vertex-ffi is free of the observability server stack"

    # The pseudosettle settlement provider runs in the wasm client. It must not
    # pull the libp2p-aware node crate; it owns its own settlement boundary types
    # instead. Assert vertex-swarm-node never appears in its dependency tree. This
    # guard is a necessary-not-sufficient marker: the actual wasm32 compile of the
    # crate is exercised by the `wasm` CI job (and the `wasm-peers` target above),
    # which is what proves it stays in the wasm cone.
    ps_tree="$(cargo tree -p vertex-swarm-bandwidth-pseudosettle -e normal --all-features)"
    if grep -q "vertex-swarm-node" <<<"$ps_tree"; then
        echo "cone guard: vertex-swarm-bandwidth-pseudosettle pulls vertex-swarm-node" >&2
        exit 1
    fi
    echo "cone guard: vertex-swarm-bandwidth-pseudosettle is free of vertex-swarm-node"

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
