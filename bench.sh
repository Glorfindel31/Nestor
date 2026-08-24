#!/bin/sh
# The nest-quality board. Not a test - a measurement harness.
# Target column is the SuperNesting job report for the same job.
N=./target/release/nest
J="--sheet 1500x1500 --margin 0 --spacing 5 --json"
row() {
  name=$1; target=$2; shift 2
  out=$($N "$@")
  printf '%-7s target %-4s ' "$name" "$target"
  echo "$out" | python -c 'import sys,json;d=json.load(sys.stdin);print("sheets %-4d util %6.2f%%  %-7s %5.1fs"%(d["sheets"],d["utilisation"],d["audit"],d["seconds"]))'
}
row test01 11 tests/fixtures/nestTest01.dxf --qty 250 --sheets 60 $J
row test02  5 tests/fixtures/nestTest02.dxf --qty 250 --sheets 60 $J
row test03  5 tests/fixtures/nestTest03.dxf --qty 250 --sheets 60 $J
row test04 63 tests/fixtures/nestTest04.dxf --qty 250 --sheets 90 $J
row test05 31 --qty 250 tests/fixtures/nestTest01.dxf tests/fixtures/nestTest02.dxf tests/fixtures/nestTest03.dxf --qty 50 tests/fixtures/nestTest04.dxf --sheets 60 $J
row ref    14 tests/fixtures/two.dxf --qty 50 --sheets 100 --sheet 2440x1220 --margin 0 --spacing 6 --generations 3 --json
