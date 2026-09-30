# fsck.ntfs: fsck(8)'s exit statuses and a JSON report that says what it
# checks, on images our own mkfs.ntfs makes: fresh (clean), made dirty with
# fs.ntfs set dirty, and damaged by hand (a mirror that disagrees with $MFT,
# an in-use record with its FILE signature destroyed).
#
# WHAT IT CHECKS is the dirty flag, $LogFile, $MFTMirr against $MFT, and the
# header of every in-use MFT record. It is not chkdsk, and the report says
# so: exit 0 means none of THESE is wrong.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

img="$SANDBOX/fsck.img"
mkfs.ntfs -q --size 64M "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"

# A fresh image: status 0 and a clean report, whichever way it is asked.
for flags in "" "-n" "-fn" "-y" "-p"; do
    # shellcheck disable=SC2086  # the words are the point
    fsck.ntfs $flags "$img" >"$SANDBOX/fsck.json" 2>"$SANDBOX/fsck.err"
    check "fsck.ntfs $flags on a fresh image exits 0 ($(cat "$SANDBOX/fsck.err"))" test $? -eq 0
    jq_check "fsck.ntfs $flags reports clean" \
        '.fs == "ntfs" and .clean == true and .dirty == false and .exit == 0 and .found == 0 and (.findings | length) == 0' \
        "$SANDBOX/fsck.json"
done
jq_check "the report names the four things it checks" \
    '.checks == ["dirty_flag", "logfile", "mft_mirror", "mft_records"]' "$SANDBOX/fsck.json"
jq_check "the report says it is not chkdsk" '.scope | test("not a full structural check") and test("chkdsk")' "$SANDBOX/fsck.json"
jq_check "the report counts the MFT records it read" '.scanned.mft_records >= 16' "$SANDBOX/fsck.json"
check "fsck.ntfs --text says clean" grep -q ': clean' <<<"$(fsck.ntfs --text "$img")"
check "rust-fs-ntfs fsck is the same program" test "$(rust-fs-ntfs fsck --text "$img")" = "$(fsck.ntfs --text "$img")"

# Dirty: reported, exit 4. -y clears it and exits 1: the log a fresh volume
# carries is marked clean, as Windows marks a cleanly dismounted one, so
# there is nothing to replay and the log is kept (#375). A log that may
# hold transactions is still refused; tests/logfile_state.rs and the unit
# tests in src/fsck.rs make one. `set dirty false` is the explicit override.
fs.ntfs "$img" set dirty true >"$SANDBOX/set.json" 2>"$SANDBOX/set.err"
check "set dirty true exits 0 ($(cat "$SANDBOX/set.err"))" test $? -eq 0
jq_check "set dirty true reports the new value" '.dirty == true' "$SANDBOX/set.json"
check "get dirty is true" test "$(fs.ntfs "$img" get dirty --text)" = true
fsck.ntfs "$img" >"$SANDBOX/dirty.json" 2>/dev/null
check "fsck.ntfs on a dirty volume exits 4" test $? -eq 4
jq_check "the dirty flag is the finding" \
    '.clean == false and .dirty == true and .exit == 4 and any(.findings[]; .kind == "dirty")' "$SANDBOX/dirty.json"
jq_check "a dirty volume with a clean log is repairable" \
    '.logfile == "clean" and (.findings[] | select(.kind == "dirty") | .repairable == true)' \
    "$SANDBOX/dirty.json"
fsck.ntfs -y "$img" >"$SANDBOX/y.json" 2>/dev/null
check "fsck.ntfs -y on a dirty volume whose log is clean exits 1" test $? -eq 1
jq_check "-y cleared the flag and kept the clean log" \
    '.repaired == 1 and .remaining == 0 and .dirty == false and .logfile == "clean"' \
    "$SANDBOX/y.json"
fsck.ntfs "$img" >/dev/null 2>&1
check "fsck.ntfs exits 0 after -y" test $? -eq 0
fs.ntfs "$img" set dirty true >/dev/null 2>&1
fs.ntfs "$img" set dirty false >/dev/null 2>&1
check "set dirty false exits 0" test $? -eq 0
check "get dirty is false again" test "$(fs.ntfs "$img" get dirty --text)" = false
fsck.ntfs "$img" >/dev/null 2>&1
check "fsck.ntfs exits 0 once the flag is cleared" test $? -eq 0
fs.ntfs "$img" set dirty maybe >/dev/null 2>"$SANDBOX/maybe.err"
check "set dirty maybe exits 2" test $? -eq 2
jq_check "set dirty maybe says what it takes" '.code == 2 and (.error | test("true or false"))' "$SANDBOX/maybe.err"

# A mirror that disagrees with $MFT: one byte of $MFTMirr's copy of record 0.
cp "$img" "$SANDBOX/mirror.img"
poke "$SANDBOX/mirror.img" $(($(mirror_offset "$SANDBOX/mirror.img" 0) + 200)) '\x5a'
fsck.ntfs "$SANDBOX/mirror.img" >"$SANDBOX/mirror.json" 2>/dev/null
check "fsck.ntfs on a disagreeing mirror exits 4" test $? -eq 4
jq_check "the mirror is the finding, record 0" \
    'any(.findings[]; .kind == "mft_mirror_mismatch" and .record == 0)' "$SANDBOX/mirror.json"

# An in-use record with its FILE signature destroyed: record 13, one of
# the reserved records mkfs marks in use.
cp "$img" "$SANDBOX/sig.img"
poke "$SANDBOX/sig.img" "$(record_offset "$SANDBOX/sig.img" 13)" 'XXXX'
fsck.ntfs -y "$SANDBOX/sig.img" >"$SANDBOX/sig.json" 2>/dev/null
check "fsck.ntfs -y on a destroyed record exits 4: nothing here repairs it" test $? -eq 4
jq_check "the record is the finding, record 13" \
    'any(.findings[]; .kind == "bad_mft_record" and .record == 13)' "$SANDBOX/sig.json"
for verb in "ls /" "get" "ls --all /"; do
    # shellcheck disable=SC2086  # the words are the point
    fs.ntfs "$SANDBOX/sig.img" $verb >/dev/null 2>"$SANDBOX/verb.err"
    status=$?
    check "fs.ntfs $verb on the damaged volume answers without a panic (status $status)" \
        test "$status" -le 1 -a "$(grep -c panicked "$SANDBOX/verb.err")" -eq 0
done

# Cannot be opened: 8, a structured error, nothing on stdout.
fsck.ntfs "$SANDBOX/absent.img" >"$SANDBOX/absent.out" 2>"$SANDBOX/absent.err"
check "a missing image exits 8" test $? -eq 8
check "a missing image prints nothing on stdout" test ! -s "$SANDBOX/absent.out"
jq_check "a missing image is a structured error with code 8" '.code == 8' "$SANDBOX/absent.err"
head -c 4096 "$img" >"$SANDBOX/cut.img"
fsck.ntfs "$SANDBOX/cut.img" >"$SANDBOX/cut.out" 2>"$SANDBOX/cut.err"
check "a truncated image exits 8" test $? -eq 8
check "a truncated image prints nothing on stdout" test ! -s "$SANDBOX/cut.out"

# A wrong command line: 16, as fsck(8) documents, not the shared 2.
fsck.ntfs -n -y "$img" >"$SANDBOX/usage.out" 2>"$SANDBOX/usage.err"
check "-n with -y exits 16" test $? -eq 16
check "-n with -y prints nothing on stdout" test ! -s "$SANDBOX/usage.out"
jq_check "-n with -y is a structured error with code 16" '.code == 16' "$SANDBOX/usage.err"

finish
