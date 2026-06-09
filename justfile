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

# Assert the default binary and wasm cones never resolve chain code
check-cone:
    #!/usr/bin/env bash
    set -euo pipefail

    # The chain (Ethereum RPC, alloy-provider, the future vertex-chain-service)
    # is a native-only, chain-enabled concern. The default vertex binary and the
    # wasm client cone must never resolve chain code through cargo feature
    # unification. This recipe locks that invariant so a later PR cannot silently
    # regress it. cargo tree -i exits non-zero when the queried crate is absent
    # from the cone, which is the passing case here.

    # Chain crates that must stay out of the default vertex binary cone. reqwest,
    # native-tls and openssl-sys are the markers the alloy chain provider drags
    # in. reqwest is handled separately below because the OTLP log exporter
    # legitimately pulls it on native today (it is absent from the wasm cone).
    chain_crates=(alloy-provider alloy-contract native-tls openssl-sys vertex-chain-service vertex-chain-api)

    echo "==> default vertex binary cone: chain crates must be absent"
    fail=0
    for c in "${chain_crates[@]}"; do
        if cargo tree -p vertex --edges normal -i "$c" >/dev/null 2>&1; then
            echo "FAIL: $c is in the default vertex cone (chain code must not reach the light node)"
            fail=1
        fi
    done
    if [ "$fail" -ne 0 ]; then exit 1; fi
    echo "    ok: default vertex cone is chain-free"

    # Advisory: reqwest is currently pulled by the OTLP HTTP log exporter in
    # vertex-observability, not by chain code. It is reported but not fatal on
    # native. Promote it to the hard list above once that exporter no longer
    # needs an HTTP client (or once reqwest's only source would be chain).
    if cargo tree -p vertex --edges normal -i reqwest >/dev/null 2>&1; then
        echo "    note: reqwest present in default vertex cone (OTLP log exporter, non-chain); not fatal"
    fi

    echo "==> feature-edge sweep: no chain feature edges in vertex"
    if cargo tree -p vertex -e features 2>/dev/null | grep -E 'alloy-provider|native-tls'; then
        echo "FAIL: a chain feature edge resolved into vertex"
        exit 1
    fi
    echo "    ok: no chain feature edges"

    # Crates documented as the wasm-safe cone in docs/agents/wasm.md. These are
    # resolved for wasm32 with default features off, which is how the browser
    # client builds. Nothing chain-flavoured, and not even reqwest, may appear.
    wasm_cone=(vertex-swarm-primitives vertex-swarm-spec vertex-swarm-forks vertex-swarm-api vertex-swarm-identity)
    wasm_forbidden=(alloy-provider alloy-contract reqwest native-tls openssl-sys vertex-chain-service vertex-chain-api)

    echo "==> wasm cone (wasm32, --no-default-features): forbidden crates must be absent"
    fail=0
    for crate in "${wasm_cone[@]}"; do
        for c in "${wasm_forbidden[@]}"; do
            if cargo tree -p "$crate" --no-default-features --target wasm32-unknown-unknown --edges normal -i "$c" >/dev/null 2>&1; then
                echo "FAIL: $c is in the wasm cone of $crate"
                fail=1
            fi
        done
    done
    if [ "$fail" -ne 0 ]; then exit 1; fi
    echo "    ok: wasm cone is chain-free and reqwest-free"

    # Best-effort wasm compile of the leaves. This currently cannot pass because
    # an upstream nectar primitive pulls wasm-bindgen-rayon, which needs a
    # threaded-wasm toolchain (atomics + bulk-memory + build-std). That is
    # tracked in docs/agents/wasm.md and is out of scope for this cone guard, so
    # the compile is advisory and never fails the recipe. The cargo tree gates
    # above are the enforced invariant.
    if rustc --print target-list 2>/dev/null | grep -qx wasm32-unknown-unknown \
        && ls "$(rustc --print sysroot)/lib/rustlib/" 2>/dev/null | grep -qx wasm32-unknown-unknown; then
        echo "==> wasm cone compile (advisory): vertex-swarm-primitives vertex-swarm-forks"
        if cargo build --target wasm32-unknown-unknown --no-default-features \
            -p vertex-swarm-primitives -p vertex-swarm-forks >/dev/null 2>&1; then
            echo "    ok: wasm leaves compiled"
        else
            echo "    note: wasm compile failed (upstream wasm-bindgen-rayon toolchain gap, see docs/agents/wasm.md); not fatal"
        fi
    else
        echo "==> wasm cone compile skipped: wasm32-unknown-unknown std not installed"
    fi

    echo "==> check-cone passed"

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
