#!/usr/bin/env bash
#
# upstream-track.sh
#
# Rebase the nxm-rs/rust-libp2p fork patch series onto upstream
# libp2p/rust-libp2p master and verify it still builds and tests cleanly.
# Alert on drift by maintaining a single tracking issue in nxm-rs/vertex.
#
# The patch series is derived at run time with `git merge-base`, never
# hard-coded: the series is every commit on the fork branch that is not an
# ancestor of upstream master. This survives upstream advancing and the
# fork's own master mirror going stale.
#
# Configuration is via environment variables (all have sane defaults):
#   FORK_REPO       Fork clone URL.
#   FORK_BRANCH     Fork branch carrying the patch series.
#   UPSTREAM_REPO   Upstream clone URL.
#   UPSTREAM_REF    Upstream ref to track (a branch name).
#   ISSUE_REPO      owner/name repo where the drift issue lives.
#   DRIFT_LABEL     Label used to find-or-create the single tracking issue.
#
# Test/local seams:
#   DRY_RUN_ISSUES=1   Log issue mutations instead of running `gh`.
#   SKIP_VERIFY=1      Skip the cargo verification stages (plumbing test only).
#   DEBUG_FORCE_FAIL=<stage>  Force the failure path at <stage> with synthetic
#                             detail, to exercise the alerting path locally.
#   KEEP_WORKDIR=1     Do not delete the temporary clone on exit.
#
# Reads (`gh ... list`) always run so an existing issue can be found; only
# writes (create/comment/close/edit/label create) honour DRY_RUN_ISSUES.

set -euo pipefail

FORK_REPO="${FORK_REPO:-https://github.com/nxm-rs/rust-libp2p.git}"
FORK_BRANCH="${FORK_BRANCH:-vertex/websocket-websys-flush-fix}"
UPSTREAM_REPO="${UPSTREAM_REPO:-https://github.com/libp2p/rust-libp2p.git}"
UPSTREAM_REF="${UPSTREAM_REF:-master}"
ISSUE_REPO="${ISSUE_REPO:-nxm-rs/vertex}"
DRIFT_LABEL="${DRIFT_LABEL:-fork-upstream-drift}"

DRY_RUN_ISSUES="${DRY_RUN_ISSUES:-0}"
SKIP_VERIFY="${SKIP_VERIFY:-0}"
DEBUG_FORCE_FAIL="${DEBUG_FORCE_FAIL:-}"
KEEP_WORKDIR="${KEEP_WORKDIR:-0}"

log() { printf '::  %s\n' "$*" >&2; }

WORKDIR=""
cleanup() {
  if [ "$KEEP_WORKDIR" != "1" ] && [ -n "$WORKDIR" ] && [ -d "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT

# Run a `gh` mutation, or log it when DRY_RUN_ISSUES=1.
mutate_gh() {
  if [ "$DRY_RUN_ISSUES" = "1" ]; then
    log "[dry-run] gh $*"
    return 0
  fi
  gh "$@"
}

ensure_label() {
  local existing
  existing="$(gh label list --repo "$ISSUE_REPO" --limit 200 --json name \
    --jq '.[].name' 2>/dev/null || true)"
  if printf '%s\n' "$existing" | grep -qx "$DRIFT_LABEL"; then
    return 0
  fi
  log "label '$DRIFT_LABEL' missing; creating it"
  # A create that loses to a concurrent create, or a label past the list page
  # limit, must not abort the run and suppress the drift alert that follows.
  if ! mutate_gh label create "$DRIFT_LABEL" --repo "$ISSUE_REPO" \
    --color "d93f0b" \
    --description "The rust-libp2p fork patch series no longer applies or verifies against upstream master"; then
    log "label create failed (it may already exist); continuing"
  fi
}

find_open_issue() {
  gh issue list --repo "$ISSUE_REPO" --label "$DRIFT_LABEL" --state open \
    --limit 1 --json number --jq '.[0].number // empty' 2>/dev/null || true
}

# open_drift_issue <upstream_sha> <stage> <detail>
open_drift_issue() {
  local sha="$1" stage="$2" detail="$3"
  local title body existing
  title="fork-drift: patch series fails against upstream rust-libp2p (${stage})"
  body=$(cat <<EOF
The nxm-rs/rust-libp2p fork patch series no longer applies cleanly onto upstream master.

- Fork branch: \`${FORK_BRANCH}\`
- Upstream ref: \`${UPSTREAM_REF}\` at \`${sha}\`
- Failing stage: \`${stage}\`

Detail:

\`\`\`
${detail}
\`\`\`

Recovery runbook: if the failing stage is the rebase and the conflict sits in the mechanical timer-sweep commit (trailer \`Regenerate: scripts/retimer.sh\`), do NOT hand-merge it. Drop that commit, rebase the remaining series, re-run \`scripts/retimer.sh\` in the fork checkout, recommit the sweep with the same trailer, and re-verify. Push the result to a candidate branch for review; never force-push the pinned branch without sign-off. Hand-merge only conflicts in the hand-written commits (the timer crate, the swarm reroll, the websocket-websys and swarm-test patches).

This issue is maintained automatically by the \`upstream-track\` workflow and will be closed when the series verifies cleanly again.
EOF
)
  ensure_label
  existing="$(find_open_issue)"
  if [ -n "$existing" ]; then
    log "updating existing drift issue #${existing}"
    mutate_gh issue edit "$existing" --repo "$ISSUE_REPO" --title "$title" --body "$body"
    mutate_gh issue comment "$existing" --repo "$ISSUE_REPO" \
      --body "Still drifting at upstream \`${sha}\`, stage \`${stage}\`."
  else
    log "creating new drift issue"
    mutate_gh issue create --repo "$ISSUE_REPO" --title "$title" --body "$body" \
      --label "$DRIFT_LABEL"
  fi
}

# resolve_drift_issue <upstream_sha>
resolve_drift_issue() {
  local sha="$1" existing
  existing="$(find_open_issue)"
  if [ -z "$existing" ]; then
    log "no open drift issue; nothing to resolve"
    return 0
  fi
  log "resolving drift issue #${existing}"
  mutate_gh issue comment "$existing" --repo "$ISSUE_REPO" \
    --body "Drift resolved: the patch series applies and verifies cleanly against upstream \`${sha}\`. Closing."
  mutate_gh issue close "$existing" --repo "$ISSUE_REPO"
}

# run_stage <stage-name> <cmd...> ; on failure sets STAGE/DETAIL and returns 1
run_stage() {
  local stage="$1"
  shift
  local out
  out="$(mktemp)"
  log "stage '${stage}': $*"
  if "$@" >"$out" 2>&1; then
    rm -f "$out"
    return 0
  fi
  STAGE="$stage"
  DETAIL="$(tail -n 40 "$out")"
  rm -f "$out"
  return 1
}

main() {
  WORKDIR="$(mktemp -d)"
  log "workdir: $WORKDIR"

  log "cloning fork branch '${FORK_BRANCH}'"
  git clone --filter=blob:none --branch "$FORK_BRANCH" "$FORK_REPO" "$WORKDIR/src" >&2
  cd "$WORKDIR/src"

  git remote add upstream "$UPSTREAM_REPO"
  git fetch --no-tags --filter=blob:none upstream "$UPSTREAM_REF" >&2

  # rerere off so a run never silently reuses a previously recorded resolution.
  git config rerere.enabled false

  local upstream_sha patch_base
  upstream_sha="$(git rev-parse "upstream/${UPSTREAM_REF}")"
  patch_base="$(git merge-base HEAD "upstream/${UPSTREAM_REF}")"
  log "upstream ${UPSTREAM_REF} = ${upstream_sha}"
  log "patch base           = ${patch_base}"
  log "patch series:"
  git --no-pager log --oneline "${patch_base}..HEAD" >&2

  STAGE=""
  DETAIL=""

  if ! git rebase --onto "upstream/${UPSTREAM_REF}" "$patch_base" >&2; then
    local conflicts
    conflicts="$(git diff --name-only --diff-filter=U || true)"
    git rebase --abort || true
    STAGE="rebase"
    DETAIL="Conflicting files:"$'\n'"${conflicts:-<none captured>}"
    log "REBASE CONFLICT"
    open_drift_issue "$upstream_sha" "$STAGE" "$DETAIL"
    return 1
  fi
  log "rebase applied cleanly onto ${upstream_sha}"

  if [ -n "$DEBUG_FORCE_FAIL" ]; then
    STAGE="$DEBUG_FORCE_FAIL"
    DETAIL="DEBUG_FORCE_FAIL set; synthetic failure at stage '${STAGE}'."
    log "DEBUG_FORCE_FAIL active"
    open_drift_issue "$upstream_sha" "$STAGE" "$DETAIL"
    return 1
  fi

  if [ "$SKIP_VERIFY" = "1" ]; then
    log "SKIP_VERIFY set; skipping cargo stages"
    resolve_drift_issue "$upstream_sha"
    return 0
  fi

  # Download every dependency up front, outside a verification stage. A
  # registry network flake then fails the job red (like a clone or fetch
  # failure) instead of masquerading as patch-series drift and filing an issue.
  log "fetching dependencies"
  cargo fetch >&2

  if ! run_stage "test" \
      cargo test -p libp2p-swarm -p libp2p-swarm-test \
    || ! run_stage "clippy" \
      cargo clippy -p libp2p-swarm -p libp2p-swarm-test --all-targets -- -D warnings \
    || ! run_stage "wasm-check" \
      cargo check -p libp2p-websocket-websys --target wasm32-unknown-unknown; then
    log "VERIFICATION FAILED at stage '${STAGE}'"
    open_drift_issue "$upstream_sha" "$STAGE" "$DETAIL"
    return 1
  fi

  log "all stages passed; series is clean against ${upstream_sha}"
  resolve_drift_issue "$upstream_sha"
  return 0
}

main "$@"
