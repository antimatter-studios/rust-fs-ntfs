#!/usr/bin/env bash
# The `box-cache` job on main fetches the box the harness pins, under the key
# the harness gives it (ci-setup-linux.sh --box-cache-key), so a kernel job's
# cache restores the box its VM boots.
#
# Since fs-linux-test-harness v0.5.0 the Vagrantfile pins each guest's box in
# a map (`"debian-12" => ["cloud-image/debian-12", "<version>"]`) and sets
# `config.vm.box` from it, so a search for a literal `config.vm.box =
# "cloud-image/..."` line finds nothing, and the job fails at `test -n`.
#
# Runs the job's own step, read from ci.yml, against the harness sibling,
# with `vagrant` stubbed to log what it is asked.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HARNESS="$ROOT/../fs-linux-test-harness"
sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
fails=0
fail() { echo "FAIL: $1" >&2; fails=$((fails + 1)); }

if [ ! -f "$HARNESS/vagrant/Vagrantfile" ]; then
    echo "FAIL: no harness sibling at $HARNESS: run 'chore siblings'" >&2
    exit 1
fi

# The step's `run: |` block, de-indented.
awk '
    /- name: Fetch the pinned box without booting a VM/ { found = 1; next }
    found && /^ *run: \|$/ { match($0, /^ */); indent = RLENGTH + 2; inrun = 1; next }
    inrun {
        if ($0 !~ /^ *$/ && match($0, /^ */) && RLENGTH < indent) exit
        print substr($0, indent + 1)
    }
' "$ROOT/.github/workflows/ci.yml" > "$sandbox/step.sh"
if ! grep -q 'box add' "$sandbox/step.sh"; then
    fail "ci.yml has a 'Fetch the pinned box without booting a VM' step that adds a box"
    exit 1
fi

mkdir -p "$sandbox/bin"
printf '#!/bin/sh\necho "vagrant $*" >> "%s/calls"\n' "$sandbox" > "$sandbox/bin/vagrant"
chmod +x "$sandbox/bin/vagrant"
: > "$sandbox/calls"
out="$(cd "$ROOT" && PATH="$sandbox/bin:$PATH" bash "$sandbox/step.sh" 2>&1)"
rc=$?
calls="$(cat "$sandbox/calls")"
[ "$rc" -eq 0 ] || fail "the step succeeds against the harness sibling (exit $rc: $out)"

key="$(cd "$ROOT" && bash "$HARNESS/scripts/ci-setup-linux.sh" --box-cache-key)" ||
    fail "the harness gives a box cache key"
box="$(sed -n 's/^vagrant box add \([^ ]*\) .*/\1/p' <<<"$calls")"
version="$(sed -n 's/.* --box-version \([^ ]*\) .*/\1/p' <<<"$calls")"
[ -n "$box" ] && [ -n "$version" ] || fail "the step adds a named box at a named version (vagrant was asked: '$calls')"
[ "$key" = "vagrant-box-${box//\//-}-${version}-amd64" ] ||
    fail "the box it adds, $box $version, is the one the cache key $key names"
case "$calls" in
    *"--provider libvirt --architecture amd64"*) ;;
    *) fail "the box is the amd64 libvirt one (vagrant was asked: '$calls')" ;;
esac

if [ "$fails" -gt 0 ]; then
    echo "box-cache-fetches-the-pinned-box: $fails check(s) failed" >&2
    exit 1
fi
echo "box-cache-fetches-the-pinned-box: all checks passed"
