#!/usr/bin/env bash
# Stop hook: run `cargo nextest run` for the workspace crates that have
# uncommitted .rs changes. Non-blocking; reports a pass/fail summary. No-op when
# nothing relevant changed or cargo/nextest are unavailable.
set -u
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$root" || exit 0
command -v cargo >/dev/null 2>&1 || exit 0
cargo nextest --version >/dev/null 2>&1 || exit 0

changed=$(git status --porcelain=v1 2>/dev/null | awk '{print $NF}' | rg '\.rs$' || true)
[ -z "$changed" ] && exit 0

meta=$(cargo metadata --no-deps --format-version 1 2>/dev/null) || exit 0
pkgs=$(printf '%s\n' "$changed" | while IFS= read -r f; do
  [ -n "$f" ] || continue
  printf '%s' "$meta" | jq -r --arg f "$root/$f" \
    '.packages[] | (.manifest_path | rtrimstr("Cargo.toml")) as $d | select($f | startswith($d)) | .name'
done | sort -u)
[ -z "$pkgs" ] && exit 0

args=(); while IFS= read -r p; do [ -n "$p" ] && args+=(-p "$p"); done <<< "$pkgs"
list=$(printf '%s' "$pkgs" | tr '\n' ' ')
if out=$(cargo nextest run --no-tests=warn "${args[@]}" 2>&1); then
  printf '{"systemMessage":%s,"suppressOutput":true}\n' "$(jq -Rn --arg m "nextest OK for touched crates: $list" '$m')"
else
  fails=$(printf '%s' "$out" | rg -N 'FAIL|error\[|test result: FAILED|panicked' | tail -15)
  printf '{"systemMessage":%s}\n' "$(jq -Rn --arg m "nextest FAILED for touched crates ($list):
$fails" '$m')"
fi
exit 0
