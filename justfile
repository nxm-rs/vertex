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
    cargo +nightly build --target wasm32-unknown-unknown -p vertex-util-runtime -p vertex-swarm-peer-score -p vertex-swarm-peer-manager

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
    # The embedded FFI client is a client, not a storer: it must never resolve
    # the storer code cone (it does not enable the builder's `reserve`).
    ffi_storer_leaked=""
    for crate in vertex-swarm-storer vertex-swarm-puller vertex-swarm-redistribution vertex-swarm-storer-behaviour; do
        if grep -q "$crate" <<<"$tree"; then
            ffi_storer_leaked="$ffi_storer_leaked $crate"
        fi
    done
    if [ -n "$ffi_storer_leaked" ]; then
        echo "cone guard: vertex-ffi pulls the storer cone:$ffi_storer_leaked" >&2
        exit 1
    fi
    echo "cone guard: vertex-ffi is free of the storer cone"
    # The default `vertex` binary is a bare client: it must not resolve the
    # storer code cone. The full storer node lives behind `--features storer`.
    default_tree="$(cargo tree -p vertex -e features)"
    storer_leaked=""
    for crate in vertex-swarm-storer vertex-swarm-puller vertex-swarm-redistribution vertex-swarm-storer-behaviour; do
        if grep -q "$crate" <<<"$default_tree"; then
            storer_leaked="$storer_leaked $crate"
        fi
    done
    if [ -n "$storer_leaked" ]; then
        echo "cone guard: default vertex pulls the storer cone:$storer_leaked" >&2
        exit 1
    fi
    echo "cone guard: default vertex is free of the storer cone"

build:
    cargo build --all-features

build-release:
    cargo build --release --all-features

# Build with tokio-console. tokio only emits the task instrumentation the
# console consumes under the tokio_unstable cfg, no longer a global rustflag.
build-tokio-console:
    RUSTFLAGS="--cfg tokio_unstable" cargo build --release --features tokio-console

# Build with the pprof CPU profiler. force-frame-pointers gives pprof readable
# flamegraphs; it is no longer a global rustflag.
build-profiling:
    RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release --features profiling

# Build every profiling feature at once; needs both opt-in rustflags. jemalloc is
# the default allocator on this platform, so heap-profiling adds its sampling.
build-profiling-all:
    RUSTFLAGS="--cfg tokio_unstable -C force-frame-pointers=yes" cargo build --release --features "profiling,heap-profiling,tokio-console"

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

# Build the release container image locally (amd64). The published image is
# multi-arch; arm64 is built in CI.
docker-build TAG="vertex:dev":
    docker build -t {{TAG}} .

# Show the cross-platform binary release plan (the five-target matrix).
dist-plan:
    dist plan

# Regenerate .github/workflows/release.yml after editing dist-workspace.toml.
dist-generate:
    dist generate
