#!/usr/bin/env bash
# scripts/fs-windows-test-harness/run-host-steps runs a matrix group's host
# steps without the VM (#546): each scenario's steps up to its first Windows
# step, as the harness builds and runs them.
#
# Black-box: the real script runs in a sandbox repository whose matrix and
# harness config are made up here, with ops that only leave evidence. What
# ran, from where, with what substituted, and what did not run is read back
# from that evidence and from the script's own lines and exit status.
#
# Quiet on success (the scripts tier is budgeted): one summary line.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/run-host-steps.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0
ok() { pass=$((pass + 1)); }
bad() {
    fail=$((fail + 1))
    printf '  FAIL  %s\n' "$1"
    [ -n "${2:-}" ] && printf '%s\n' "$2" | sed 's/^/        /'
    return 0
}

repo="$tmp/repo"
ev="$tmp/evidence"
mkdir -p "$repo/scripts/fs-windows-test-harness" "$ev"
cp "$ROOT/scripts/fs-windows-test-harness/run-host-steps" "$repo/scripts/fs-windows-test-harness/"

cat >"$repo/fs-windows-test-harness.toml" <<EOF
[groups]
good = ["made", "conditional"]
all = ["made", "conditional", "wrong-exit", "needs-vm", "not-in-matrix"]

[ops.make]
host = "host"
command = "mkdir -p {image_dir}/{run_id} && printf '%s %s %s {{x}}' {scenario.volume_params.size_mib} '{scenario.volume_params.label}' {step.what} >{image_dir}/{run_id}/{scenario.image} && pwd >$ev/made.pwd && cp {image_dir}/{run_id}/{scenario.image} $ev/made.image && echo {scenario.image} >$ev/made.name"
expect_exit = 0

[ops.three]
host = "host"
command = "test -s {image_dir}/{run_id}/{scenario.image} && touch $ev/three.ran && exit 3"
expect_exit = 3

[ops.after]
host = "host"
command = "touch $ev/after.ran"

[ops.win]
host = "vm"
command = "touch $ev/win.ran"

[ops.maybe]
host = "host"
when = "step.enabled"
command = "touch $ev/maybe.ran"

[ops.fails]
host = "host"
command = "echo the reason it failed >&2; exit 1"

[ops.vmpath]
host = "host"
command = "touch $ev/vmpath.ran {vm.workdir}/x"
EOF

cat >"$repo/test-matrix.json" <<'EOF'
{
  "scenarios": {
    "made": {
      "volume_params": { "size_mib": 64, "label": "A B", "alloc_unit_size": 4096 },
      "image": "img-{scenario.volume_params.size_mib}-{N}.img",
      "recipe": [
        { "op": "make", "what": "first" },
        { "op": "three" },
        { "op": "ship-to-vm", "src": "x", "dest": "y" },
        { "op": "win" },
        { "op": "after" }
      ]
    },
    "conditional": {
      "volume_params": { "size_mib": 1, "label": "C", "alloc_unit_size": 4096 },
      "image": "c.img",
      "recipe": [ { "op": "maybe" }, { "op": "maybe", "enabled": "" } ]
    },
    "wrong-exit": {
      "volume_params": { "size_mib": 1, "label": "W", "alloc_unit_size": 4096 },
      "image": "w.img",
      "recipe": [ { "op": "fails" }, { "op": "after" } ]
    },
    "needs-vm": {
      "volume_params": { "size_mib": 1, "label": "V", "alloc_unit_size": 4096 },
      "image": "v.img",
      "recipe": [ { "op": "vmpath" } ]
    }
  }
}
EOF

run() { out="$("$repo/scripts/fs-windows-test-harness/run-host-steps" --group "$1" 2>&1)"; status=$?; }

# ── 1. A group whose host steps all exit as expected ───────────────────────
run good
[ "$status" -eq 0 ] && ok || bad "a group whose host steps pass exits 0 (exit $status)" "$out"
grep -q '^ok   made: 2 host steps$' <<<"$out" && ok || bad "the scenario reports the two steps before ship-to-vm" "$out"
grep -q '^test result: ok\. 2 passed; 0 failed (good: 2 host steps run;' <<<"$out" && ok ||
    bad "the result line, in cargo's form, counts scenarios and the steps run" "$out"
[ "$(cat "$ev/made.pwd" 2>/dev/null)" = "$repo" ] && ok || bad "steps run from the repository root" "$(cat "$ev/made.pwd" 2>/dev/null)"
[ "$(cat "$ev/made.image" 2>/dev/null)" = "64 A B first {x}" ] && ok ||
    bad "scenario and step references are substituted, {{ and }} are braces" "$(cat "$ev/made.image" 2>/dev/null)"
[ "$(cat "$ev/made.name" 2>/dev/null)" = "img-64-{N}.img" ] && ok ||
    bad "a scenario string is expanded in turn, and its own {N} is text" "$(cat "$ev/made.name" 2>/dev/null)"
[ -e "$ev/three.ran" ] && ok || bad "a step expecting exit 3 runs and passes on 3"
[ ! -e "$ev/win.ran" ] && ok || bad "a step on the VM is not run"
[ ! -e "$ev/after.ran" ] && ok || bad "a host step after the VM's steps is not run"
grep -q '^windows made: step 4 (after) runs after the VM.s step 2, so the matrix job runs it$' <<<"$out" && ok ||
    bad "a host step after the VM's steps is named as the matrix job's" "$out"
[ ! -e "$ev/maybe.ran" ] && ok || bad "an op whose when is false or empty is not run, as in the harness"
[ ! -e "$repo/tmp/host-steps" ] && ok || bad "the images are removed" "$(ls -R "$repo/tmp" 2>/dev/null)"

# ── 2. A group with failures ───────────────────────────────────────────────
rm -f "$ev"/*
run all
[ "$status" -eq 1 ] && ok || bad "a group with a failing step exits 1 (exit $status)" "$out"
grep -q '^FAIL wrong-exit: step 0 (fails): exit 1, expected 0$' <<<"$out" && ok ||
    bad "a wrong exit fails the scenario, naming the step" "$out"
grep -q 'the reason it failed' <<<"$out" && ok || bad "a failing step's output is shown" "$out"
[ ! -e "$ev/after.ran" ] && ok || bad "a scenario stops at its failing step"
grep -q '^FAIL needs-vm: step 0 (vmpath): needs {vm.workdir}, which only the harness provides$' <<<"$out" && ok ||
    bad "a reference only the harness provides fails the step" "$out"
[ ! -e "$ev/vmpath.ran" ] && ok || bad "a step that cannot be built completely is not run"
grep -q '^FAIL not-in-matrix: in group' <<<"$out" && ok || bad "a group member missing from the matrix fails" "$out"
grep -q '^test result: FAILED\. 2 passed; 3 failed (all: 3 host steps run;' <<<"$out" && ok ||
    bad "the summary counts the failures, and a failing step as run" "$out"

# ── 3. A group under its floor ─────────────────────────────────────────────
out="$("$repo/scripts/fs-windows-test-harness/run-host-steps" --group good --floor 3 2>&1)"; status=$?
[ "$status" -eq 1 ] && grep -q "^FAIL group 'good' has 2 scenarios, under its floor of 3" <<<"$out" && ok ||
    bad "a group under its floor fails (exit $status)" "$out"

# ── 4. A group that does not exist ─────────────────────────────────────────
run nosuch
[ "$status" -eq 2 ] && ok || bad "an unknown group exits 2 (exit $status)" "$out"

printf 'run-host-steps: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
