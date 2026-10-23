# fs.ntfs write, mkdir and set label, as installed: round trips compared
# with cmp at every size that changes how NTFS stores a file (empty,
# resident in the MFT record, one cluster, one cluster and a byte, a MiB),
# a file in a subdirectory, replacements by shorter and longer content,
# typed ls entries, the label, refusals, and a clean fsck.ntfs afterwards.
# mkdir --parents: a missing chain made whole, under existing directories,
# through names in another case and through `.` and `..` read as Windows
# reads them; a chain already there, made by nothing; and every refusal,
# with no partial chain and the image byte-identical. What Windows makes of
# the chains is the cli-mkdir-parents-win-verify-chkdsk scenario in
# test-matrix.json. Then a volume with a user file's record destroyed:
# every verb answers without a panic, and fsck.ntfs reports the record.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

# random FILE N: N random bytes (0 allowed, which `head -c` refuses on macOS).
random() { if [ "$2" -gt 0 ]; then head -c "$2" /dev/urandom >"$1"; else : >"$1"; fi; }

img="$SANDBOX/write.img"
mkfs.ntfs -q --text --size 64M --label CLITEST "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"

fs.ntfs "$img" mkdir /d >"$SANDBOX/mkdir.json" 2>"$SANDBOX/mkdir.err"
status=$?
check "mkdir /d exits 0 ($(cat "$SANDBOX/mkdir.err"))" test "$status" -eq 0
jq_check "mkdir reports the path and a numeric record" '.path == "/d" and (.record | type) == "number"' "$SANDBOX/mkdir.json"
fs.ntfs "$img" mkdir /d/e >/dev/null 2>&1
check "mkdir /d/e exits 0" test $? -eq 0

# Every size boundary a 4 KiB cluster and a 4 KiB record have, and a MiB.
for size in 0 1 500 4096 4097 1048576; do
    random "$SANDBOX/src.$size" "$size"
    fs.ntfs "$img" write "/f$size" <"$SANDBOX/src.$size" >"$SANDBOX/w.json" 2>"$SANDBOX/w.err"
    status=$?
    check "write /f$size exits 0 ($(cat "$SANDBOX/w.err"))" test "$status" -eq 0
    jq_check "write /f$size reports $size bytes, created" ".bytes == $size and .created == true" "$SANDBOX/w.json"
    fs.ntfs "$img" read "/f$size" >"$SANDBOX/back.$size" 2>/dev/null
    check "read /f$size matches what was written" cmp -s "$SANDBOX/src.$size" "$SANDBOX/back.$size"
done
random "$SANDBOX/deep" 5000
fs.ntfs "$img" write /d/e/deep <"$SANDBOX/deep" >/dev/null 2>&1
check "write /d/e/deep exits 0" test $? -eq 0
check "read /d/e/deep matches" cmp -s "$SANDBOX/deep" <(fs.ntfs "$img" read /d/e/deep)

# Replaced, shorter and longer, across the resident / non-resident line:
# the old content must not survive, and nothing of the new may be lost.
for pair in "4097 1" "1048576 500" "1 4097" "4096 1048576" "500 0" "0 4096"; do
    set -- $pair
    fs.ntfs "$img" write "/f$1" <"$SANDBOX/src.$2" >"$SANDBOX/r.json" 2>"$SANDBOX/r.err"
    status=$?
    check "replace /f$1 by $2 bytes exits 0 ($(cat "$SANDBOX/r.err"))" test "$status" -eq 0
    jq_check "replacing /f$1 reports $2 bytes, not created" ".bytes == $2 and .created == false" "$SANDBOX/r.json"
    check "read /f$1 is the $2-byte content" cmp -s "$SANDBOX/src.$2" <(fs.ntfs "$img" read "/f$1")
done

# ls on a populated volume: every entry typed, the directory a directory.
fs.ntfs "$img" ls / >"$SANDBOX/ls.json" 2>/dev/null
jq_check "every ls entry has typed fields" \
    'length == 7 and all(.[]; (.name|type)=="string" and (.type|type)=="string" and (.size|type)=="number" and (.mode|test("^[0-7]{4}$")) and (.mtime|type)=="number" and (.record|type)=="number")' \
    "$SANDBOX/ls.json"
jq_check "ls / reports each file at its new size" \
    'any(.[]; .name == "f4097" and .size == 1) and any(.[]; .name == "f4096" and .size == 1048576) and any(.[]; .name == "d" and .type == "dir")' \
    "$SANDBOX/ls.json"
fs.ntfs "$img" ls /d >"$SANDBOX/lsd.json" 2>/dev/null
jq_check "ls /d shows e as a directory" '[.[] | select(.name == "e" and .type == "dir")] | length == 1' "$SANDBOX/lsd.json"

# The label: a space survives (the case text output breaks), the 32-unit
# limit is enforced before anything is written, and "" removes it.
fs.ntfs "$img" set label "Backup Volume" >"$SANDBOX/label.json" 2>"$SANDBOX/label.err"
status=$?
check "set label 'Backup Volume' exits 0 ($(cat "$SANDBOX/label.err"))" test "$status" -eq 0
check "get label --text is 'Backup Volume'" test "$(fs.ntfs "$img" get label --text)" = "Backup Volume"
cp "$img" "$SANDBOX/label-before.img"
fs.ntfs "$img" set label "$(printf 'x%.0s' $(seq 33))" >/dev/null 2>"$SANDBOX/long.err"
check "a 33-unit label exits 2" test $? -eq 2
jq_check "a 33-unit label names the limit" '.code == 2 and (.error | test("limit is 32"))' "$SANDBOX/long.err"
check "a refused label wrote nothing" cmp -s "$img" "$SANDBOX/label-before.img"
fs.ntfs "$img" set label "" >/dev/null 2>&1
jq_check "set label \"\" removes it" '.label == null' <(fs.ntfs "$img" get label)
fs.ntfs "$img" set label "Backup Volume" >/dev/null 2>&1

# Refusals: a structured error, nothing on stdout, image unchanged; status 4
# where the volume's state refuses a namespace verb (diskjockey#413), 1 for write.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" mkdir /d >"$SANDBOX/x.out" 2>"$SANDBOX/x.err"
check "mkdir of an existing path exits 4" test $? -eq 4
check "mkdir of an existing path prints nothing on stdout" test ! -s "$SANDBOX/x.out"
jq_check "mkdir of an existing path says so" '.code == 4 and (.error | test("already exists"))' "$SANDBOX/x.err"
echo x | fs.ntfs "$img" write /missing/f >"$SANDBOX/y.out" 2>"$SANDBOX/y.err"
check "write under a missing parent exits 1" test $? -eq 1
check "write under a missing parent prints nothing on stdout" test ! -s "$SANDBOX/y.out"
jq_check "write under a missing parent is a structured error" '.code == 1 and (.error | test("not found"))' "$SANDBOX/y.err"
echo x | fs.ntfs "$img" write /d >/dev/null 2>"$SANDBOX/z.err"
jq_check "write over a directory is refused" '.code == 1 and (.error | test("is a directory"))' "$SANDBOX/z.err"
check "the refusals left the image as it was" cmp -s "$img" "$SANDBOX/before.img"

# A new file the volume cannot hold is refused, and is not left behind
# empty: the created path is removed again.
head -c 100000000 /dev/zero | fs.ntfs "$img" write /toobig >/dev/null 2>"$SANDBOX/big.err"
check "a write larger than the volume exits 1" test $? -eq 1
jq_check "a write larger than the volume is a structured error" '.code == 1 and (.error | type) == "string"' "$SANDBOX/big.err"
fs.ntfs "$img" ls /toobig >/dev/null 2>&1
check "the refused new file is not left on the volume" test $? -eq 1

# A dirty volume is not written to.
cp "$img" "$SANDBOX/dirty.img"
fs.ntfs "$SANDBOX/dirty.img" set dirty true >/dev/null 2>&1
cp "$SANDBOX/dirty.img" "$SANDBOX/dirty-before.img"
echo x | fs.ntfs "$SANDBOX/dirty.img" write /new >/dev/null 2>"$SANDBOX/d.err"
jq_check "a write to a dirty volume is refused, saying why" '.code == 1 and (.error | test("dirty"))' "$SANDBOX/d.err"
check "the refused write changed nothing" cmp -s "$SANDBOX/dirty.img" "$SANDBOX/dirty-before.img"

# The volume is clean afterwards, by our checker and by the flag.
fsck.ntfs "$img" >"$SANDBOX/fsck.json" 2>/dev/null
check "fsck.ntfs after the writes exits 0" test $? -eq 0
check "the volume is not dirty after the writes" test "$(fs.ntfs "$img" get dirty --text)" = false

# A volume Windows was writing to when it was captured, its $LogFile
# holding work Windows had not yet written home (dirty flag clear, as
# Windows 8 and later leave it). Windows leaves the same after a Fast
# Startup shutdown, when replaying the log corrupts the volume, so a write
# refuses it and names the replay to ask for: `fsck.ntfs -y` (#137). After
# that replay every file Windows listed after its own recovery of the same
# image is there at Windows' size and SHA-256, the new file beside them,
# and fsck.ntfs finds nothing left to do.
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -d' ' -f1; }
mid="$SANDBOX/interrupted.img"
gzip -dc "$REPO/test-disks/windows-interrupted-1.img.gz" >"$mid"
check "the fixture volume Windows left mid-write unpacked" test -s "$mid"
random "$SANDBOX/after" 5000
cp "$mid" "$SANDBOX/interrupted.before"
# The status is taken before the message is built: a command substitution
# in check's arguments resets $? before `test $?` reads it.
fs.ntfs "$mid" write /after-replay <"$SANDBOX/after" >/dev/null 2>"$SANDBOX/mid.err"
status=$?
check "write over a log holding work exits 1 (exit $status)" test "$status" -eq 1
jq_check "the refusal names \$LogFile and fsck.ntfs -y" \
    '.error | contains("$LogFile") and contains("fsck.ntfs -y")' "$SANDBOX/mid.err"
check "a refused write leaves the image as it was" cmp -s "$mid" "$SANDBOX/interrupted.before"
fsck.ntfs -y "$mid" >/dev/null 2>"$SANDBOX/mid-replay.err"
status=$?
check "fsck.ntfs -y replays the log (exit $status, $(cat "$SANDBOX/mid-replay.err"))" test "$status" -le 1
fs.ntfs "$mid" write /after-replay <"$SANDBOX/after" >/dev/null 2>"$SANDBOX/mid.err"
status=$?
check "write after the replay exits 0 ($(cat "$SANDBOX/mid.err"))" test "$status" -eq 0
check "the file written after the replay reads back" cmp -s "$SANDBOX/after" <(fs.ntfs "$mid" read /after-replay)
fsck.ntfs "$mid" >"$SANDBOX/mid-fsck.json" 2>/dev/null
status=$?
check "fsck.ntfs finds nothing after the write (exit $status)" test "$status" -eq 0
jq_check "fsck.ntfs calls the log clean" '.logfile == "clean"' "$SANDBOX/mid-fsck.json"
listed=0
differ=""
while IFS=$'\t' read -r path size sum; do
    [ -n "$path" ] || continue
    listed=$((listed + 1))
    fs.ntfs "$mid" read "/$path" >"$SANDBOX/one" 2>/dev/null
    if [ "$(wc -c <"$SANDBOX/one" | tr -d ' ')" != "$size" ] || [ "$(sha256 <"$SANDBOX/one")" != "$sum" ]; then
        differ="$differ /$path"
    fi
done <"$REPO/test-disks/windows-interrupted-1.recovered.manifest"
check "Windows' manifest lists the workload's files ($listed)" test "$listed" -gt 200
check "every file Windows recovered reads back at Windows' size and SHA-256 (differ:${differ:0:300})" test -z "$differ"

# The same volume with a page of the work torn: the log cannot be replayed
# in full, so the write is refused and not a byte of the image changes.
torn="$SANDBOX/torn.img"
gzip -dc "$REPO/test-disks/windows-interrupted-1.img.gz" >"$torn"
# $LogFile is one run at LCN 10318; its record page at 0x80000 is needed,
# and byte 510 is the first sector's update sequence copy.
at=$((10318 * 4096 + 0x80000 + 510))
poke "$torn" "$at" "\\x$(printf '%02x' $(($(u8 "$torn" "$at") ^ 255)))"
cp "$torn" "$SANDBOX/torn.before"
fs.ntfs "$torn" write /after-replay <"$SANDBOX/after" >/dev/null 2>"$SANDBOX/torn.err"
status=$?
check "write over a log it cannot replay in full exits 1 (exit $status)" test "$status" -eq 1
jq_check "the refusal names \$LogFile" '.error | contains("$LogFile")' "$SANDBOX/torn.err"
check "a refused write leaves the image as it was" cmp -s "$torn" "$SANDBOX/torn.before"

# mkdir --parents, on a volume of its own.
pimg="$SANDBOX/parents.img"
mkfs.ntfs -q --size 64M --label PARENTS "$pimg" >/dev/null 2>&1
check "mkfs.ntfs made the --parents image" test -s "$pimg"
echo x | fs.ntfs "$pimg" write /f.txt >/dev/null 2>&1
check "write /f.txt exits 0" test $? -eq 0

# made PATH CREATED...: mkdir -p PATH succeeds, reporting {"path": PATH,
# "created": [CREATED...]}, and each CREATED is a directory that stat
# finds and its parent lists, spelled exactly so.
made() {
    local path="$1" want dir
    shift
    want="$(printf '%s\n' "$@" | jq -R . | jq -sc .)"
    fs.ntfs "$pimg" mkdir -p "$path" >"$SANDBOX/p.json" 2>"$SANDBOX/p.err"
    local rc=$?
    check "mkdir -p $path exits 0 (got $rc: $(cat "$SANDBOX/p.err"))" test "$rc" -eq 0
    check "mkdir -p $path prints nothing on stderr" test ! -s "$SANDBOX/p.err"
    jq_check "mkdir -p $path reports what it made, $want" \
        ". == {\"path\": \"$path\", \"created\": $want}" "$SANDBOX/p.json"
    for dir in "$@"; do
        jq_check "$dir is a directory" '.type == "dir"' <(fs.ntfs "$pimg" stat "$dir" 2>/dev/null)
        local parent="${dir%/*}"
        check "ls ${parent:-/} lists ${dir##*/}, exactly so" \
            grep -qxF -- "${dir##*/}" <<<"$(fs.ntfs "$pimg" ls "${parent:-/}" 2>/dev/null | jq -r '.[].name')"
    done
}
# there PATH: mkdir -p PATH succeeds with nothing to make, and writes nothing.
there() {
    cp "$pimg" "$SANDBOX/p-before.img"
    fs.ntfs "$pimg" mkdir -p "$1" >"$SANDBOX/p.json" 2>"$SANDBOX/p.err"
    local rc=$?
    check "mkdir -p $1, already there, exits 0 (got $rc: $(cat "$SANDBOX/p.err"))" test "$rc" -eq 0
    jq_check "mkdir -p $1 reports nothing created" ". == {\"path\": \"$1\", \"created\": []}" "$SANDBOX/p.json"
    check "mkdir -p $1 leaves the image byte-identical" cmp -s "$pimg" "$SANDBOX/p-before.img"
}
# refused STATUS PATTERN PATH: mkdir -p PATH exits STATUS with nothing on
# stdout, a structured error matching PATTERN, and the image byte-identical,
# so no part of the chain is left behind.
refused() {
    cp "$pimg" "$SANDBOX/p-before.img"
    fs.ntfs "$pimg" mkdir -p "$3" >"$SANDBOX/p.out" 2>"$SANDBOX/p.err"
    local rc=$?
    check "mkdir -p $3 exits $1 (got $rc: $(cat "$SANDBOX/p.err"))" test "$rc" -eq "$1"
    check "mkdir -p $3 prints nothing on stdout" test ! -s "$SANDBOX/p.out"
    jq_check "mkdir -p $3 says why" ".code == $1 and (.error | test(\"$2\"; \"i\"))" "$SANDBOX/p.err"
    check "mkdir -p $3 leaves the image byte-identical" cmp -s "$pimg" "$SANDBOX/p-before.img"
}

made /a/b/c /a /a/b /a/b/c                            # a three-level chain
made /a/b/d/e /a/b/d /a/b/d/e                         # under existing directories
made /A/B/NewDir /A/B/NewDir                          # through names in another case
check "the case-blind walk made no second b in /a" \
    test "$(fs.ntfs "$pimg" ls /a 2>/dev/null | jq '[.[] | select(.name | ascii_downcase == "b")] | length')" = 1
made /a/./x/../y /a/y                                 # `.` and `..` by the text, as Windows reads them
check "/a/./x/../y made no /a/x" test -z "$(fs.ntfs "$pimg" ls /a 2>/dev/null | jq -r '.[] | select(.name == "x") | .name')"
made /f.txt/../g /g                                   # `..` after a file, as Windows reads it
made "/na$(printf '\303\257')ve/$(printf '\346\227\245\346\234\254')" \
    "/na$(printf '\303\257')ve" "/na$(printf '\303\257')ve/$(printf '\346\227\245\346\234\254')"
made //deep//1/2/3/4/5/6/7/8/9/10/11/12/ \
    /deep /deep/1 /deep/1/2 /deep/1/2/3 /deep/1/2/3/4 /deep/1/2/3/4/5 /deep/1/2/3/4/5/6 \
    /deep/1/2/3/4/5/6/7 /deep/1/2/3/4/5/6/7/8 /deep/1/2/3/4/5/6/7/8/9 \
    /deep/1/2/3/4/5/6/7/8/9/10 /deep/1/2/3/4/5/6/7/8/9/10/11 /deep/1/2/3/4/5/6/7/8/9/10/11/12

there /a/b/c
there /a/b/c/
there /A/B/C
there /a/./b/..
there /
fs.ntfs "$pimg" mkdir --parents /a/b/c >/dev/null 2>&1
check "--parents is -p's long form" test $? -eq 0
check "mkdir -p --text names each directory made" \
    test "$(fs.ntfs "$pimg" mkdir -p --text /t1/t2 2>/dev/null)" = "$(printf 'created /t1\ncreated /t1/t2')"
check "mkdir -p --text says when nothing was made" \
    test "$(fs.ntfs "$pimg" mkdir -p --text /t1/t2 2>/dev/null)" = "/t1/t2 exists; nothing created"

fsck.ntfs "$pimg" >/dev/null 2>&1
check "fsck.ntfs finds the volume clean after the chains" test $? -eq 0
check "the volume is not dirty after the chains" test "$(fs.ntfs "$pimg" get dirty --text)" = false

# Something in the way is refused before a directory is made, as Windows
# refuses it: a file on the way or at PATH, and a symbolic link, which is
# not followed.
fs.ntfs "$pimg" ln -s /a /lnk >/dev/null 2>&1
check "ln -s /a /lnk exits 0" test $? -eq 0
refused 4 'not a directory' /f.txt/x/y
refused 4 'not a directory' /new/../f.txt/x
refused 4 'already exists' /f.txt
refused 4 'already exists' /F.TXT
refused 4 'symbolic link' /lnk/x
refused 4 'already exists' /lnk
# A name Windows cannot address, past a directory that would be made first.
refused 1 'cannot address' '/n1/a:b/c'
refused 1 'cannot address' '/n1/a*b'
refused 1 'trailing space or period' /n1/x./y
refused 1 'trailing space or period' '/n1/x /y'
refused 1 'NUL' /n1/nul
refused 1 'too long' "/n1/$(printf '%0256d' 0)/z"
# shellcheck disable=SC2016 # \$Extend is the metafile's name, not a variable.
refused 1 'metafile' '/$Extend/n1'
check "no refused chain left /n1 or /new behind" \
    test -z "$(fs.ntfs "$pimg" ls / 2>/dev/null | jq -r '.[] | select(.name == "n1" or .name == "new") | .name')"
fs.ntfs "$pimg" mkdir -p n1/n2 >"$SANDBOX/p.out" 2>/dev/null
check "mkdir -p of a path not from the root is a usage error" test $? -eq 2
check "mkdir -p of a path not from the root prints nothing on stdout" test ! -s "$SANDBOX/p.out"

# Plain mkdir is unchanged: its parent must exist.
fs.ntfs "$pimg" mkdir /m1/m2 >"$SANDBOX/p.out" 2>"$SANDBOX/p.err"
check "plain mkdir under a missing parent exits 4" test $? -eq 4
jq_check "plain mkdir under a missing parent says so" '.code == 4 and (.error | test("not found"))' "$SANDBOX/p.err"

# --offset: the same volume 1 MiB into a whole-disk image, the first MiB
# left alone.
{ head -c 1048576 /dev/zero; cat "$pimg"; } >"$SANDBOX/whole.img"
fs.ntfs --offset 1048576 "$SANDBOX/whole.img" mkdir -p /o1/o2 >"$SANDBOX/p.json" 2>"$SANDBOX/p.err"
rc=$?
check "mkdir -p through --offset exits 0 (got $rc: $(cat "$SANDBOX/p.err"))" test "$rc" -eq 0
jq_check "mkdir -p through --offset made the chain" '.created == ["/o1", "/o1/o2"]' "$SANDBOX/p.json"
jq_check "mkdir -p through --offset made it inside the volume" '.type == "dir"' \
    <(fs.ntfs --offset 1048576 "$SANDBOX/whole.img" stat /o1/o2 2>/dev/null)
check "mkdir -p through --offset wrote nothing before the volume" \
    cmp -s -n 1048576 "$SANDBOX/whole.img" /dev/zero

# A dirty volume asks for a check before anything is written to it.
fs.ntfs "$pimg" set dirty true >/dev/null 2>&1
check "set dirty true exits 0" test $? -eq 0
refused 1 'dirty' /d1/d2

# A user file's MFT record with its FILE signature destroyed, and a mirror
# that disagrees with $MFT: the volume is damaged, and every verb must say
# so or carry on -- never panic -- and fsck.ntfs must report it unclean.
bad="$SANDBOX/bad.img"
cp "$img" "$bad"
victim="$(fs.ntfs "$bad" ls /f4097 | jq '.[0].record')"
poke "$bad" "$(record_offset "$bad" "$victim")" 'BAD!'
poke "$bad" $(($(mirror_offset "$bad" 1) + 300)) '\x5a'
for verb in "ls /" "ls /f4097" "read /f4097" "read /f1" "get" "info" "ls /d"; do
    # shellcheck disable=SC2086  # the words are the point
    fs.ntfs "$bad" $verb >/dev/null 2>"$SANDBOX/v.err"
    status=$?
    if [ "$status" -eq 0 ]; then
        ok
    else
        jq_check "fs.ntfs $verb on the damaged volume fails as a structured error (status $status)" \
            ".code == $status and (.error | type) == \"string\"" "$SANDBOX/v.err"
    fi
    check "fs.ntfs $verb on the damaged volume does not panic" test "$(grep -c panicked "$SANDBOX/v.err")" -eq 0
done
fs.ntfs "$bad" read /f4097 >/dev/null 2>"$SANDBOX/victim.err"
check "reading the destroyed file fails" test $? -eq 1
fsck.ntfs "$bad" >"$SANDBOX/bad.json" 2>/dev/null
check "fsck.ntfs on the damaged volume exits 4" test $? -eq 4
jq_check "fsck.ntfs reports the destroyed record and the mirror" \
    "any(.findings[]; .kind == \"bad_mft_record\" and .record == $victim) and any(.findings[]; .kind == \"mft_mirror_mismatch\" and .record == 1)" \
    "$SANDBOX/bad.json"

finish
