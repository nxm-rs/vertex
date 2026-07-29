#!/usr/bin/env bash
# PostToolUse(Write|Edit): format the edited Rust file with rustfmt (edition 2024).
# Fast per-file format; no-op for non-.rs paths or when rustfmt is unavailable.
set -u
f=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null) || exit 0
case "$f" in *.rs) ;; *) exit 0 ;; esac
[ -f "$f" ] || exit 0
command -v rustfmt >/dev/null 2>&1 || exit 0
rustfmt --edition 2024 "$f" 2>/dev/null || true
exit 0
