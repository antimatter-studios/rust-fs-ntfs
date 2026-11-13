# fs.ntfs mv, as installed: a name renamed within its directory -- to a
# name of the same length, a longer one and a shorter one, a case-only
# rename, a directory, a file whose data is not resident, a Unicode name --
# keeping the file's record and bytes; a rename onto itself that writes
# nothing; --text and --offset; and every refusal, each leaving the image
# byte-identical: a cross-directory move (exit 3, which this driver cannot
# do), a name already there as Windows compares names, a missing source,
# the names Windows cannot address, the root, `.`, a metafile, a trailing
# `/` on a file, a dirty volume and the usage errors. What Windows makes of
# a renamed volume is the cli-rename-win-verify-chkdsk scenario in
# test-matrix.json.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/mv.img"
mkfs.ntfs -q --size 64M --label CLIMV "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
for dir in /d /e; do
    fs.ntfs "$img" mkdir "$dir" >/dev/null 2>&1
    check "mkdir $dir exits 0" test $? -eq 0
done
head -c 300 /dev/urandom >"$SANDBOX/small"
head -c 50000 /dev/urandom >"$SANDBOX/large"
for spec in /a.txt:small /big.bin:large /d/f.txt:small /e/taken.txt:small /naive.txt:small; do
    fs.ntfs "$img" write "${spec%:*}" <"$SANDBOX/${spec##*:}" >/dev/null 2>&1
    check "write ${spec%:*} exits 0" test $? -eq 0
done

# record PATH: the MFT record stat reports for PATH, or nothing.
record() { fs.ntfs "$img" stat "$1" 2>/dev/null | jq -r '.inode // empty'; }
# names DIR: the names ls DIR lists, one per line.
names() { fs.ntfs "$img" ls "$1" 2>/dev/null | jq -r '.[].name'; }

# moved SOURCE PATH BYTES: mv SOURCE PATH succeeds and reports PATH; PATH
# holds BYTES (a file in $SANDBOX) in SOURCE's record, and SOURCE's name is
# gone from the directory. BYTES `-` is a directory.
moved() {
    local source="$1" path="$2" bytes="$3" was
    was="$(record "$source")"
    fs.ntfs "$img" mv "$source" "$path" >"$SANDBOX/mv.json" 2>"$SANDBOX/mv.err"
    local rc=$?
    check "mv $source $path exits 0 (got $rc: $(cat "$SANDBOX/mv.err"))" test "$rc" -eq 0
    check "mv $source $path prints nothing on stderr" test ! -s "$SANDBOX/mv.err"
    jq_check "mv $source $path reports {\"path\": \"$path\", \"moved\": true}" \
        ". == {\"path\": \"$path\", \"moved\": true}" "$SANDBOX/mv.json"
    check "$path is $source's record ($was)" test -n "$was" -a "$(record "$path")" = "$was"
    local dir="${path%/*}"
    check "ls ${dir:-/} lists ${path##*/}, exactly so" grep -qxF -- "${path##*/}" <<<"$(names "${dir:-/}")"
    check "ls ${dir:-/} no longer lists ${source##*/}" \
        test -z "$(grep -xF -- "${source##*/}" <<<"$(names "${dir:-/}")")"
    if [ "$bytes" != - ]; then
        fs.ntfs "$img" read "$path" >"$SANDBOX/read" 2>/dev/null
        check "read $path gives the bytes written to $source" cmp -s "$SANDBOX/read" "$SANDBOX/$bytes"
    fi
}

moved /a.txt /b.txt small                     # the same length
moved /b.txt /a-much-longer-name.txt small    # longer
moved /a-much-longer-name.txt /s.txt small    # shorter
moved /big.bin /large-renamed.bin large       # data that is not resident
moved /s.txt /S.TXT small                     # case only
moved /d /docs -                              # a directory
moved /docs/f.txt /docs/g.txt small           # inside a directory
moved /naive.txt "/na$(printf '\303\257')ve-$(printf '\346\227\245\346\234\254').txt" small
fs.ntfs "$img" read /docs/g.txt >"$SANDBOX/read" 2>/dev/null
check "the renamed directory still holds its file" cmp -s "$SANDBOX/read" "$SANDBOX/small"

fsck.ntfs "$img" >"$SANDBOX/fsck.json" 2>/dev/null
check "fsck.ntfs finds the volume clean after the renames" test $? -eq 0

# A name onto itself is a rename that changes nothing: it succeeds and
# writes nothing, as Windows' MoveFile does.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" mv /S.TXT /S.TXT >"$SANDBOX/self.json" 2>"$SANDBOX/self.err"
rc=$?
check "mv of a name onto itself exits 0 (got $rc: $(cat "$SANDBOX/self.err"))" test "$rc" -eq 0
jq_check "mv of a name onto itself reports the path" '. == {"path": "/S.TXT", "moved": true}' "$SANDBOX/self.json"
check "mv of a name onto itself leaves the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"

# --text, for people.
check "mv --text says what moved where" \
    test "$(fs.ntfs "$img" mv --text /docs/g.txt /docs/h.txt 2>/dev/null)" = "moved /docs/g.txt to /docs/h.txt"

# --offset: the same volume 1 MiB into a whole-disk image, the first MiB
# left alone.
{ head -c 1048576 /dev/zero; cat "$img"; } >"$SANDBOX/whole.img"
fs.ntfs --offset 1048576 "$SANDBOX/whole.img" mv /docs/h.txt /docs/i.txt >/dev/null 2>"$SANDBOX/off.err"
rc=$?
check "mv through --offset exits 0 (got $rc: $(cat "$SANDBOX/off.err"))" test "$rc" -eq 0
check "mv through --offset renamed the name inside the volume" \
    grep -qxF i.txt <<<"$(fs.ntfs --offset 1048576 "$SANDBOX/whole.img" ls /docs 2>/dev/null | jq -r '.[].name')"
check "mv through --offset wrote nothing before the volume" \
    cmp -s -n 1048576 "$SANDBOX/whole.img" /dev/zero

# refused STATUS PATTERN ARG...: fs.ntfs IMG mv ARG... exits STATUS with
# nothing on stdout, a structured error matching PATTERN, and the image
# byte-identical.
refused() {
    local status="$1" pattern="$2"
    shift 2
    cp "$img" "$SANDBOX/before.img"
    fs.ntfs "$img" mv "$@" >"$SANDBOX/x.out" 2>"$SANDBOX/x.err"
    local rc=$?
    check "mv $* exits $status (got $rc: $(cat "$SANDBOX/x.err"))" test "$rc" -eq "$status"
    check "mv $* prints nothing on stdout" test ! -s "$SANDBOX/x.out"
    jq_check "mv $* says why" ".code == $status and (.error | test(\"$pattern\"; \"i\"))" "$SANDBOX/x.err"
    check "mv $* leaves the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"
}

# Another directory is a move this driver cannot make: exit 3, by name.
refused 3 'not implemented.*director' /S.TXT /e/S.TXT
refused 3 'not implemented.*director' /e/taken.txt /taken.txt
refused 3 'not implemented.*director' /docs /e/docs
# A name already there, as Windows compares names: through $UpCase, so
# case does not tell two names apart.
refused 4 'already exists' /S.TXT /large-renamed.bin
refused 4 'already exists' /S.TXT /LARGE-RENAMED.BIN
refused 4 'already exists' /docs /E
# What is not there, and a parent that is not a directory.
refused 4 'not found' /missing.txt /other.txt
refused 4 'not found' /S.TXT /nowhere/S.TXT
refused 4 'not a directory' /S.TXT /S.TXT/x
# Names Windows cannot address.
refused 1 'cannot address' /S.TXT '/a:b'
refused 1 'cannot address' /S.TXT '/a*b'
refused 1 'trailing space or period' /S.TXT /s.
refused 1 'NUL' /S.TXT /nul
refused 1 'too long' /S.TXT "/$(printf '%0256d' 0)"
# What is not a name to rename: the root, `.`, `..` and a metafile.
refused 1 'root' / /x
refused 1 'cannot be renamed' /docs/. /docs/x
refused 1 'cannot be renamed' /S.TXT /docs/..
# shellcheck disable=SC2016 # \$MFT is the metafile's name, not a variable.
refused 1 'metafile' '/$MFT' /x
# A trailing `/` says the name is a directory.
refused 4 'not a directory' /S.TXT/ /T.TXT
refused 4 'not a directory' /S.TXT /T.TXT/

# A directory named with a trailing `/` is still itself.
fs.ntfs "$img" mv /docs/ /docs2/ >"$SANDBOX/slash.json" 2>"$SANDBOX/slash.err"
rc=$?
check "mv /docs/ /docs2/ exits 0 (got $rc: $(cat "$SANDBOX/slash.err"))" test "$rc" -eq 0
check "ls / lists docs2 and not docs after mv /docs/ /docs2/" \
    test "$(names / | grep -xE 'docs2?' | tr '\n' ' ')" = "docs2 "

# The usage errors: exit 2, nothing on stdout.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" mv /S.TXT >"$SANDBOX/u.out" 2>/dev/null
check "mv without PATH is a usage error" test $? -eq 2
check "mv without PATH prints nothing on stdout" test ! -s "$SANDBOX/u.out"
fs.ntfs "$img" mv S.TXT /T.TXT >"$SANDBOX/u.out" 2>"$SANDBOX/u.err"
check "mv of a path not from the root is a usage error" test $? -eq 2
check "mv of a path not from the root prints nothing on stdout" test ! -s "$SANDBOX/u.out"
fs.ntfs "$img" mv /S.TXT T.TXT >/dev/null 2>&1
check "mv to a path not from the root is a usage error" test $? -eq 2
check "the usage errors leave the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"

# A dirty volume asks for a check before anything is written to it.
fs.ntfs "$img" set dirty true >/dev/null 2>&1
check "set dirty true exits 0" test $? -eq 0
refused 1 'dirty' /S.TXT /T.TXT

check "mv --help carries an example" grep -q '^Examples:' <<<"$(fs.ntfs x mv --help 2>/dev/null)"

finish
