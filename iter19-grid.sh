#!/usr/bin/env bash
# Iteration 19 grid: (mode, K, footprint) -> steady KB/s, notconn%, maxWs, verify.
# mode=range isolates Lever 1 (footprint only, byte-range, no address bias);
# mode=addr adds Lever 2 (address-biased workers + address-sharded chunks).
set -uo pipefail
cd /tmp/demo-nodefix
REF="439fdc2c403dfedc3bde7ea122240856ca13c6417b4dc26d705e808f37bc1fe9"
PATH_M="__up__/hoverfly/hoverfly_bg.wasm"
EXPECT_SHA="d6669caea28e70f628ae8859e8a58e2cffceaee75c7d0194abdd496b43b173c6"
GRID="${GRID:?set GRID as 'mode:K:footprint:bootstrap ...'}"
SUM="${SUM:-/tmp/demo-nodefix/iter19-summary.txt}"
: > "$SUM"
for spec in $GRID; do
  IFS=':' read -r MODE K FP BOOT <<< "$spec"
  OUTD="/tmp/demo-nodefix/grid19/${MODE}-k${K}-fp${FP}"
  rm -rf "$OUTD"
  OUT="$OUTD" REF="$REF" PATH_IN_MANIFEST="$PATH_M" MODE="$MODE" K="$K" WIDTH="${WIDTH:-0}" \
    FOOTPRINT="$FP" BOOTSTRAP="$BOOT" \
    WARMUP="${WARMUP:-18000}" RUN_TIMEOUT_MS="${RTO:-200000}" HARD_TIMEOUT_MS="${HTO:-300000}" \
    node shard-range-harness.mjs >/dev/null 2>&1
  if [ -f "$OUTD/result.json" ]; then
    node -e '
      const fs=require("fs"); const r=JSON.parse(fs.readFileSync(process.argv[1])); const exp=process.argv[2];
      if(r.error){console.log(process.argv[3]+" ERROR="+r.error+" maxWs="+(r.maxWs||"?"));process.exit(0);}
      const ok=r.byteComplete&&r.wasmMagic&&r.sha256===exp;
      const ls=r.legStats||{};
      console.log([process.argv[3],"kbps="+r.kbps,"mbps="+r.mbps,"fetchS="+r.fetchSecs,
        "notconn%="+(ls.notconnPct??"?"),"spc="+(ls.substreamsPerChunk??"?"),
        "maxWs="+r.maxWs,"wsOpened="+r.totalWsOpened,"VERIFIED="+ok].join(" "));
    ' "$OUTD/result.json" "$EXPECT_SHA" "$spec" | tee -a "$SUM"
  else
    echo "$spec NO-RESULT" | tee -a "$SUM"
  fi
done
echo "=== iter19 grid written: $SUM ===" | tee -a "$SUM"
