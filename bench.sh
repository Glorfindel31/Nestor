#!/bin/sh
# The nest-quality board. Not a test - a measurement harness.
#
#   sh bench.sh                 one run per row, seed 0
#   RUNS=3 sh bench.sh          three runs per row: min/median seconds
#   SEED=7 sh bench.sh          another seed (sheet counts are one seed's luck)
#
# Builds `nest` first, so it never measures a stale binary. Exits 1 when a row
# fails to run, fails its audit, or uses more sheets than the previous run
# recorded in bench-last.txt (gitignored).
#
# Target column: SuperNesting's job report for the same job (2026-08, the
# comparison runs in PLAN.md section 1). `ref` is 14 here, but PLAN.md section 1
# says 13 - unresolved, and only ever printed, never gated on.
set -eu
cargo build --release --bin nest --quiet
N=./target/release/nest
PY=$(command -v python3 || command -v python || command -v py) || { echo "bench.sh: needs python" >&2; exit 1; }
RUNS=${RUNS:-1}
SEED=${SEED:-0}
LAST=bench-last.txt
NEW=$(mktemp)
J="--sheet 1500x1500 --margin 0 --spacing 5 --json"
fail=0

row() {
  name=$1; target=$2; shift 2
  outs=""
  i=0
  while [ "$i" -lt "$RUNS" ]; do
    status=0
    out=$($N "$@" --seed "$SEED") || status=$?
    # 2 = ran fine, audit FAILED: still reported, so the row says why.
    if [ "$status" -ne 0 ] && [ "$status" -ne 2 ]; then
      echo "$name: nest exited $status" >&2
      fail=1
      return
    fi
    outs="$outs$out
"
    i=$((i + 1))
  done
  line=$(printf '%s' "$outs" | "$PY" -c '
import sys, json, statistics
runs = [json.loads(l) for l in sys.stdin if l.strip()]
d = runs[0]
secs = [r["seconds"] for r in runs]
print("%d %s %.2f %.1f %.1f" % (max(r["sheets"] for r in runs), d["audit"], d["utilisation"], min(secs), statistics.median(secs)))
')
  set -- $line
  printf '%-7s target %-4s sheets %-4s util %6s%%  %-7s min %5ss  median %5ss\n' "$name" "$target" "$1" "$3" "$2" "$4" "$5"
  echo "$name $1 $2" >> "$NEW"
  if [ "$2" != "PASSED" ]; then
    echo "$name: audit $2" >&2
    fail=1
  fi
  if [ -f "$LAST" ]; then
    before=$(awk -v n="$name" '$1 == n { print $2 }' "$LAST")
    if [ -n "$before" ] && [ "$1" -gt "$before" ]; then
      echo "$name: $1 sheets, was $before" >&2
      fail=1
    fi
  fi
}

row test01 11 tests/fixtures/nestTest01.dxf --qty 250 --sheets 60 $J
row test02  5 tests/fixtures/nestTest02.dxf --qty 250 --sheets 60 $J
row test03  5 tests/fixtures/nestTest03.dxf --qty 250 --sheets 60 $J
row test04 63 tests/fixtures/nestTest04.dxf --qty 250 --sheets 90 $J
row test05 31 --qty 250 tests/fixtures/nestTest01.dxf tests/fixtures/nestTest02.dxf tests/fixtures/nestTest03.dxf --qty 50 tests/fixtures/nestTest04.dxf --sheets 60 $J
row ref    14 tests/fixtures/two.dxf --qty 50 --sheets 100 --sheet 2440x1220 --margin 0 --spacing 6 --generations 3 --json
# Curves and chained LINE/ARC profiles at zero clearance: the audit at margin 0
# is where two shipped bugs hid (PLAN.md section 6). No commercial target.
row curvy   - tests/fixtures/curvy.dxf --qty 40 --sheets 20 --sheet 1500x1500 --margin 0 --spacing 0 --json
row chained - tests/fixtures/chained.dxf --qty 40 --sheets 20 --sheet 1500x1500 --margin 0 --spacing 0 --json

# Only a clean board becomes the new reference, so a regression can't hide by
# being recorded.
if [ "$fail" -eq 0 ]; then mv "$NEW" "$LAST"; else rm -f "$NEW"; fi
exit "$fail"
