#!/usr/bin/env bash
# A sibling that is a git WORKTREE counts as checked out.
#
# Every working copy in this family is a worktree, a sibling at a pinned ref
# included. A worktree's `.git` is a FILE naming its gitdir, so a
# `[ -d "$dir/.git" ]` test reads every worktree as missing: `siblings:check`
# reports it MISSING and fails the gate every build and test tier depends
# on, and `siblings` tries to clone over it.
#
# This runs the real `siblings` and `siblings:check` task bodies out of
# chores.yml in a sandbox where one sibling is a worktree of a scratch
# repository at a tag and the other is an ordinary clone, and requires both
# to be reported present and at the pinned ref. Then the clone is removed,
# and `siblings` must fetch it afresh.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHORES="$ROOT/chores.yml"
pass=0
fail=0

ok()   { pass=$((pass + 1)); }
bad()  { fail=$((fail + 1)); printf 'FAIL %s\n' "$*"; }

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

# Nothing from the caller's git configuration reaches the sandbox.
export GIT_CONFIG_GLOBAL="$sandbox/gitconfig" GIT_CONFIG_NOSYSTEM=1
git config --global user.name test
git config --global user.email test@example.invalid
git config --global init.defaultBranch main
git config --global commit.gpgsign false
git config --global tag.gpgsign false

origin="$sandbox/origin"

# The first `- |` script body of a task, dedented, with its sibling URLs and
# pinned refs pointed at the sandbox.
task_body() {
    awk -v t="  $1:" '
        $0 == t { task = 1; next }
        task && /^  [a-z][a-z:_-]*:$/ { exit }
        task && /^      - \|$/ { inside = 1; next }
        task && inside && /^      - / { exit }
        task && inside { sub(/^        /, ""); print }
    ' "$CHORES" | sed -E "s#https://[^ ]+\.git#$origin#g; s#\{\{\.[A-Z_]+_REF\}\}#v1#g"
}

sync="$(task_body siblings)"
check="$(task_body siblings:check)"
[ -n "$sync" ] && ok || bad "chores.yml has a siblings task with a script body"
[ -n "$check" ] && ok || bad "chores.yml has a siblings:check task with a script body"
case "$sync$check" in
    *'{{'*) bad "every template variable in the siblings tasks was substituted" ;;
esac

names="$(printf '%s\n' "$check" | sed -nE "s/^ *([a-z0-9-]+) +'v1'.*/\1/p")"
[ -n "$names" ] || { bad "siblings:check names its siblings"; exit 1; }
first="$(printf '%s\n' "$names" | head -n 1)"
last="$(printf '%s\n' "$names" | tail -n 1)"

# --- A scratch upstream: v1 is tagged, main is one commit past it. ---------
git init -q "$origin"
git -C "$origin" commit -q --allow-empty -m one
git -C "$origin" tag v1
git -C "$origin" commit -q --allow-empty -m two
git -C "$origin" remote add origin "$origin"

# The checkout the tasks run from; siblings are `../<name>` beside it.
root="$sandbox/root"
git init -q "$root/this"
git -C "$root/this" commit -q --allow-empty -m this

# The first sibling is a worktree at the tag; every other one is a clone.
git -C "$origin" worktree add -q --detach "$root/$first" v1
for n in $names; do
    [ "$n" = "$first" ] && continue
    git clone -q "$origin" "$root/$n"
    git -C "$root/$n" checkout -q v1
done
[ -f "$root/$first/.git" ] && ok || bad "the fixture's $first is a worktree (.git is a file)"

run() { (cd "$root/this" && bash -c "$1") 2>&1; }

# siblings:check: every sibling present and at the pin, gate open.
out="$(run "$check")"; rc=$?
[ "$rc" -eq 0 ] && ok || bad "siblings:check passes when a sibling is a worktree (rc=$rc): $out"
for n in $names; do
    printf '%s\n' "$out" | grep -qE "^  $n +ok " && ok \
        || bad "siblings:check reports $n ok; it said: $out"
done
case "$out" in
    *MISSING*) bad "siblings:check reported a checked-out sibling MISSING: $out" ;;
    *) ok ;;
esac

# siblings: nothing to do, and certainly no clone over the worktree.
out="$(run "$sync")"; rc=$?
[ "$rc" -eq 0 ] && ok || bad "siblings exits 0 when a sibling is a worktree (rc=$rc): $out"
case "$out" in
    *"cloning $first"*) bad "siblings tried to clone over the $first worktree: $out" ;;
    *) ok ;;
esac
[ "$(git -C "$root/$first" rev-parse HEAD)" = "$(git -C "$origin" rev-parse v1)" ] && ok \
    || bad "the $first worktree was left at v1"

# A sibling that is genuinely missing is reported, and then fetched.
if [ "$last" != "$first" ]; then
    rm -rf "${root:?}/$last"
    out="$(run "$check")"; rc=$?
    [ "$rc" -ne 0 ] && ok || bad "siblings:check fails when $last is missing"
    printf '%s\n' "$out" | grep -qE "^  $last +MISSING" && ok \
        || bad "siblings:check reports the missing $last; it said: $out"
    out="$(run "$sync")"; rc=$?
    [ "$rc" -eq 0 ] && ok || bad "siblings exits 0 fetching a missing sibling (rc=$rc): $out"
    case "$out" in
        *"cloning $last at v1"*) ok ;;
        *) bad "siblings fetches the missing $last; it said: $out" ;;
    esac
    out="$(run "$check")"; rc=$?
    [ "$rc" -eq 0 ] && ok || bad "siblings:check passes once $last is fetched: $out"
fi

# --- No other guard in chores.yml asks for a .git DIRECTORY. ---------------
dir_tests="$(grep -nE -- '-d "[^"]*/\.git"' "$CHORES" || true)"
[ -z "$dir_tests" ] && ok || bad "chores.yml tests for a .git directory, which a worktree does not have:
$dir_tests"

printf 'siblings worktree: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
