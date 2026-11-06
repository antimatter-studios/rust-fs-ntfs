#!/usr/bin/env bash
# scripts/run-matrix.sh takes a lock directory per argument list, so two runs
# of the same scenarios cannot race (#1283).
#
# The lock's name used to be every forwarded argument, joined and sanitised.
# A long filter or a long --ssh-key or --vm-workdir path took it past
# NAME_MAX (255 bytes on macOS and Linux), so mkdir always failed, and the
# run then blamed a stale lock it had never seen. windows-job-diagnostics.sh
# hit it whenever TMPDIR was long, because its sandbox path reaches
# --ssh-key.
#
# Black-box: the real run-matrix.sh runs in a sandbox beside a stand-in
# harness. The stand-in finds the lock directory holding its parent's pid,
# which is run-matrix.sh's, and records its name, so the test reads the name
# the run actually took rather than recomputing it.
#
# Quiet on success (the scripts tier is budgeted): one summary line.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/run-matrix-lock-name.XXXXXX")"
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

mkdir -p "$tmp/consumer/scripts" "$tmp/fs-windows-test-harness/scripts" "$tmp/bin"
cp "$ROOT/scripts/run-matrix.sh" "$tmp/consumer/scripts/run-matrix.sh"
# No VM_HOST: run-matrix.sh starts no ssh mux, and nothing leaves the sandbox.
: > "$tmp/consumer/.test-env"

# The harness: record the name of the lock directory its parent holds.
cat > "$tmp/fs-windows-test-harness/scripts/run-tests.sh" <<'EOF'
#!/usr/bin/env bash
for d in /tmp/ntfs-matrix-lock-*; do
    [ "$(cat "$d/pid" 2>/dev/null)" = "$PPID" ] && basename "$d" >> "$LOCK_LOG"
done
exit 0
EOF

# A mkdir that refuses to make a lock directory for a reason other than the
# lock existing, as a full or read-only /tmp would. Every other call is the
# real mkdir.
real_mkdir="$(command -v mkdir)"
cat > "$tmp/bin/mkdir" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *ntfs-matrix-lock-*)
        if [ -n "\${REFUSE_LOCK_MKDIR:-}" ]; then
            echo "mkdir: \${@: -1}: Permission denied" >&2
            exit 1
        fi ;;
esac
exec "$real_mkdir" "\$@"
EOF
chmod +x "$tmp/bin/mkdir"

# run_matrix ARGS... -- run the sandboxed run-matrix.sh; the lock it took is
# the last line of lock.log, its status rc, its output out.log.
run_matrix() {
    : > "$tmp/lock.log"
    LOCK_LOG="$tmp/lock.log" PATH="$tmp/bin:$PATH" \
        bash "$tmp/consumer/scripts/run-matrix.sh" "$@" > "$tmp/out.log" 2>&1
    rc=$?
    lock="$(tail -n 1 "$tmp/lock.log")"
}

# Every case's arguments start with this, so its locks cannot collide with a
# real run's or another test's.
tag="run-matrix-lock-name-$$"

# ── 1. A long argument list still takes a lock within NAME_MAX ──────────────
# The arguments windows-job-diagnostics.sh passes, from a sandbox under a long
# TMPDIR: 258 bytes of lock name before the fix.
long_dir="$tmp/$(printf 'a%.0s' $(seq 1 120))/windows-job-diagnostics.XXXXXX/green"
long_args=("$tag-foreign-fragmented-indx-512-win-enumerate-chkdsk"
    --vm-host runneradmin@localhost --ssh-key "$long_dir/key"
    --vm-workdir C:/rust-fs-ntfs-matrix)
run_matrix "${long_args[@]}"
long_lock="$lock"
if [ "$rc" -eq 0 ] && [ -n "$long_lock" ]; then
    ok
else
    bad "a long argument list: the run takes its lock and reaches the harness (rc=$rc)" \
        "$(cat "$tmp/out.log")"
fi
if [ -n "$long_lock" ] && [ "${#long_lock}" -le 255 ]; then
    ok
else
    bad "a long argument list: the lock name is at most 255 bytes (${#long_lock})" "$long_lock"
fi

# ── 2. Different arguments still take different locks ──────────────────────
# Two lists that differ only past any prefix a name could keep...
run_matrix "${long_args[@]}" --extra-a
lock_a="$lock"
run_matrix "${long_args[@]}" --extra-b
lock_b="$lock"
if [ -n "$lock_a" ] && [ -n "$lock_b" ] && [ "$lock_a" != "$lock_b" ] \
    && [ "$lock_a" != "$long_lock" ]; then
    ok
else
    bad "lists differing only at their end take different locks" \
        "$long_lock"$'\n'"$lock_a"$'\n'"$lock_b"
fi
# ...and two that sanitise to the same characters.
run_matrix "$tag-x/y"
lock_slash="$lock"
run_matrix "$tag-x_y"
lock_under="$lock"
if [ -n "$lock_slash" ] && [ -n "$lock_under" ] && [ "$lock_slash" != "$lock_under" ]; then
    ok
else
    bad "'x/y' and 'x_y' take different locks" "$lock_slash"$'\n'"$lock_under"
fi

# ── 3. The same arguments are still refused while their lock is held ────────
run_matrix "$tag-held"
held="$lock"
if [ -n "$held" ]; then
    mkdir "/tmp/$held"
    sleep 300 &
    holder=$!
    echo "$holder" > "/tmp/$held/pid"
    run_matrix "$tag-held"
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    rm -rf "/tmp/${held:?}"
    if [ "$rc" -ne 0 ] && grep -q "is already running (pid $holder)" "$tmp/out.log"; then
        ok
    else
        bad "a lock held by a live run refuses the same arguments (rc=$rc)" "$(cat "$tmp/out.log")"
    fi
else
    bad "a lock held by a live run refuses the same arguments: no lock was taken to hold" \
        "$(cat "$tmp/out.log")"
fi

# ── 4. A mkdir that fails for another reason says that reason ───────────────
REFUSE_LOCK_MKDIR=1 run_matrix "$tag-refused"
log="$(cat "$tmp/out.log")"
if [ "$rc" -ne 0 ]; then
    ok
else
    bad "a lock mkdir refused: the run stops (rc=$rc)" "$log"
fi
case "$log" in
    *"Permission denied"*) ok ;;
    *) bad "a lock mkdir refused: the run reports mkdir's own reason" "$log" ;;
esac
case "$log" in
    *stale*) bad "a lock mkdir refused: the run does not blame a stale lock" "$log" ;;
    *) ok ;;
esac

if [ "$fail" -eq 0 ]; then
    echo "run-matrix-lock-name.sh: all checks passed ($pass)"
else
    printf 'run-matrix-lock-name.sh: %d passed, %d failed\n' "$pass" "$fail"
fi
[ "$fail" -eq 0 ]
