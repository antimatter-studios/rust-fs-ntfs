#!/usr/bin/env bash
# Tests for the Windows fixture producers' shared helper (issue #629).
#
# capture-filename-fields.ps1 once called New-WindowsFixture without loading
# it, and the hosted Windows run failed with "not recognized" (run
# 38837437553). That is a text-level contract a Linux runner can hold before a
# Windows runner is spent: the helper is defined once, in a file with no
# top-level side effects, and every producer that calls it loads that file.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

pass=0; fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf '  FAIL  %s\n' "$1"; }

helper=test-disks/windows-fixture.ps1
producers=(test-disks/build-windows-native-read-fixtures.ps1 test-disks/capture-filename-fields.ps1)
workflow=.github/workflows/security-descriptor-oracle.yml

printf 'windows-fixture-producers\n'

[ "$(grep -c '^function New-WindowsFixture' "$helper")" -eq 1 ] \
    && ok || bad "$helper defines New-WindowsFixture exactly once"

# Outside the function body the helper holds only comments and blank lines,
# so dot-sourcing it cannot build, mount or delete anything.
top_level=$(awk '/^function /{inside=1} inside && /^}/{inside=0; next} !inside && !/^[[:space:]]*(#|$)/' "$helper")
[ -z "$top_level" ] && ok || bad "$helper has top-level statements: $top_level"

for p in "${producers[@]}"; do
    grep -q '^\. "\$PSScriptRoot\\windows-fixture\.ps1"' "$p" \
        && ok || bad "$p dot-sources windows-fixture.ps1"
    if grep -q '^function New-WindowsFixture' "$p"; then bad "$p redefines New-WindowsFixture"; else ok; fi
done

grep -q 'Assert-VolumeClean -Root \$root -Step \$step' test-disks/capture-filename-fields.ps1 \
    && ok || bad "every capture stage runs chkdsk while the volume is mounted"
grep -q 'chkdsk needs a drive letter' test-disks/capture-filename-fields.ps1 \
    && ok || bad "a stage without a drive letter fails by name"
grep -q 'has no chkdsk log' "$workflow" \
    && ok || bad "$workflow requires a chkdsk log per step"

# Cleanup runs after the upload, on failure too, and names only the six
# generated stage images.
upload_line=$(grep -n 'name: filename-fields-' "$workflow" | head -1 | cut -d: -f1)
cleanup_line=$(grep -n 'generated stage images are removed after the artifact handoff' "$workflow" | cut -d: -f1)
if [ -n "$upload_line" ] && [ -n "$cleanup_line" ] && [ "$cleanup_line" -gt "$upload_line" ]; then ok
else bad "$workflow removes the stage images after the artifact upload"; fi
cleanup=$(sed -n "${cleanup_line:-1},\$p" "$workflow" | sed -n '1,10p')
grep -q 'if: always()' <<<"$cleanup" && ok || bad "the cleanup step runs on failure too"
grep -q 'Remove-Item -LiteralPath "test-disks/ntfs-fn-fields-step\$step.img"' <<<"$cleanup" \
    && ok || bad "the cleanup removes only the named stage images"
if grep -qE 'Remove-Item[^|]*\*' <<<"$cleanup"; then bad "the cleanup uses a wildcard"; else ok; fi

printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
