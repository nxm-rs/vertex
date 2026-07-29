#!/usr/bin/env bash
# PostToolUse(Write|Edit): flag edits that introduce banned tokens (house style).
# vertex bans em-dashes and the word "underlay" in source, rustdoc, and markdown (AGENTS.md).
set -u
f=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null) || exit 0
case "$f" in *.rs|*.md) ;; *) exit 0 ;; esac
[ -f "$f" ] || exit 0
if rg -qF $'\xe2\x80\x94' "$f"; then
  printf '{"decision":"block","reason":%s}\n' \
    "$(jq -Rn --arg r "Em-dash found in $f. This repo bans em-dashes (AGENTS.md): use ASCII hyphens or split the sentence." '$r')"
  exit 0
fi
if rg -qiw underlay "$f"; then
  printf '{"decision":"block","reason":%s}\n' \
    "$(jq -Rn --arg r "Say 'multiaddrs', never 'underlay' (AGENTS.md). File: $f" '$r')"
  exit 0
fi
exit 0
