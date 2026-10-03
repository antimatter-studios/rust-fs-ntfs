#!/usr/bin/env bash
# The Windows chkdsk gate runs on every change that can alter what it checks
# (#403): format code, the matrix, the workflow -- and the pins of the tools
# the Windows job runs. A harness bump changes every command the matrix sends
# to Windows; before this test, `HARNESS_REF` moving skipped the gate, so the
# pull request that moved it was never run against Windows at all.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/needs-windows-gate.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
pass=0
fail=0

git -C "$WORK" init -q -b main
git -C "$WORK" config user.email test@example.invalid
git -C "$WORK" config user.name test
mkdir -p "$WORK/src" "$WORK/scripts" "$WORK/tests/scripts"
cat > "$WORK/chores.yml" <<'YML'
vars:
  FS_CORE_REF: v0.2.16
  HARNESS_REF: v4.2.0
  IMG_VHD_REF: v0.3.0
tasks:
  build:
    desc: build it
YML
echo 'fn main() {}' > "$WORK/src/mkfs.rs"
echo 'fn main() {}' > "$WORK/src/read.rs"
echo '#!/bin/sh' > "$WORK/scripts/run-matrix.sh"
echo '# x' > "$WORK/tests/scripts/chkdsk-verdict.ps1"
echo '# x' > "$WORK/tests/scripts/other.ps1"
git -C "$WORK" add -A
git -C "$WORK" commit -q -m base
base="$(git -C "$WORK" rev-parse HEAD)"

# case <want> <label> <shell edit run in the scratch repo>
case_is() {
    local want="$1" label="$2" edit="$3" got
    git -C "$WORK" checkout -q -B "case" "$base"
    ( cd "$WORK" && eval "$edit" )
    git -C "$WORK" commit -q -am "$label"
    got="$(cd "$WORK" && bash "$ROOT/scripts/needs-windows-gate.sh" "$base")"
    if [ "$got" = "$want" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL %s: got %s, want %s\n' "$label" "$got" "$want"
    fi
}

case_is true  "format code"                 "echo '// x' >> src/mkfs.rs"
case_is false "read-path code"              "echo '// x' >> src/read.rs"
case_is true  "the harness pin moves"       "sed -i.bak 's/HARNESS_REF: v4.2.0/HARNESS_REF: v4.3.0/' chores.yml && rm chores.yml.bak"
case_is true  "the VHD writer pin moves"    "sed -i.bak 's/IMG_VHD_REF: v0.3.0/IMG_VHD_REF: v0.3.1/' chores.yml && rm chores.yml.bak"
case_is false "an unrelated chores.yml line" "sed -i.bak 's/desc: build it/desc: build it all/' chores.yml && rm chores.yml.bak"
case_is true  "the matrix wrapper"          "echo '# x' >> scripts/run-matrix.sh"
case_is true  "the chkdsk verdict's test"   "echo '# y' >> tests/scripts/chkdsk-verdict.ps1"
case_is false "another script test"         "echo '# y' >> tests/scripts/other.ps1"

if [ "$fail" -gt 0 ]; then
    echo "needs-windows-gate: $fail of $((pass + fail)) failed"
    exit 1
fi
echo "needs-windows-gate: $pass cases"
