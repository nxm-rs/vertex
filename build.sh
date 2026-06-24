#!/usr/bin/env bash
# Rebuild the demo wasm via the develop-shell rust (has wasm32 std) + trunk binary.
set -euo pipefail
cd /tmp/demo-nodefix/bin/swarm-demo
nix develop /tmp/demo-nodefix --command bash -c \
  'export PATH="$PATH:/nix/store/crhskjdjqg94vhpmk174lj3xyfkghq90-trunk-0.21.14/bin"; trunk build --release 2>&1 | tail -4'
