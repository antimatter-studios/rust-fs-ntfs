# fs.ntfs ln, as installed: a hard link -- in the same directory, in
# another, to a file whose data is not resident, under a Unicode name --
# through which the file's bytes read the same and after which every name
# counts one more link; a name removed leaving the others; symbolic links
# whose targets are relative, from the root, a drive path and a name not
# there, each read back as given; --text and --offset; and every refusal,
# each leaving the image byte-identical: a name already there as Windows
# compares names, a missing target or directory, a hard link to a
# directory, a file whose record has no room for another name and one
# already at Windows' 1024 links, the names Windows cannot address, the
# root, `.`, `..`, a metafile, an empty or oversized symbolic link target,
# a trailing `/`, a dirty volume and the usage errors. What Windows makes
# of the links is the cli-link-win-verify-chkdsk scenario in
# test-matrix.json.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

img="$SANDBOX/ln.img"
mkfs.ntfs -q --size 64M --label CLILN "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
for dir in /d /e; do
    fs.ntfs "$img" mkdir "$dir" >/dev/null 2>&1
    check "mkdir $dir exits 0" test $? -eq 0
done
head -c 300 /dev/urandom >"$SANDBOX/small"
head -c 50000 /dev/urandom >"$SANDBOX/large"
for spec in /a.txt:small /big.bin:large /d/f.txt:small /full.txt:small /max.txt:small; do
    fs.ntfs "$img" write "${spec%:*}" <"$SANDBOX/${spec##*:}" >/dev/null 2>&1
    check "write ${spec%:*} exits 0" test $? -eq 0
done

# jq_is ARG...: jq -e ARG... holds, printing nothing.
jq_is() { jq -e "$@" >/dev/null 2>&1; }
# record PATH: the MFT record stat reports for PATH, or nothing.
record() { fs.ntfs "$img" stat "$1" 2>/dev/null | jq -r '.inode // empty'; }
# nlink PATH: the links stat counts for PATH, or nothing.
nlink() { fs.ntfs "$img" stat "$1" 2>/dev/null | jq -r '.nlink // empty'; }

# linked FILE PATH BYTES LINKS: ln FILE PATH succeeds and reports PATH and
# FILE's record; PATH is FILE's record and reads BYTES (a file in
# $SANDBOX), and both names now count LINKS.
linked() {
    local file="$1" path="$2" bytes="$3" links="$4" was
    was="$(record "$file")"
    fs.ntfs "$img" ln "$file" "$path" >"$SANDBOX/ln.json" 2>"$SANDBOX/ln.err"
    local rc=$?
    check "ln $file $path exits 0 (got $rc: $(cat "$SANDBOX/ln.err"))" test "$rc" -eq 0
    check "ln $file $path prints nothing on stderr" test ! -s "$SANDBOX/ln.err"
    check "ln $file $path reports {\"path\": \"$path\", \"linked\": true, \"inode\": $was}: $(cat "$SANDBOX/ln.json")" \
        jq_is --arg p "$path" --arg r "$was" '. == {"path": $p, "linked": true, "inode": ($r | tonumber)}' "$SANDBOX/ln.json"
    check "$path is $file's record ($was)" test -n "$was" -a "$(record "$path")" = "$was"
    check "$path and $file each count $links links" test "$(nlink "$path") $(nlink "$file")" = "$links $links"
    fs.ntfs "$img" read "$path" >"$SANDBOX/read" 2>/dev/null
    check "read $path gives the bytes written to $file" cmp -s "$SANDBOX/read" "$SANDBOX/$bytes"
}

linked /a.txt /a2.txt small 2                  # the same directory
linked /a.txt /d/a3.txt small 3                # another directory
linked /big.bin /e/big-link.bin large 2        # data that is not resident
linked /d/f.txt "/e/na$(printf '\303\257')ve-$(printf '\346\227\245\346\234\254').txt" small 2

# A name removed leaves the file under its others.
fs.ntfs "$img" rm /a.txt >/dev/null 2>&1
check "rm /a.txt exits 0" test $? -eq 0
fs.ntfs "$img" read /a2.txt >"$SANDBOX/read" 2>/dev/null
check "read /a2.txt still gives the bytes after rm /a.txt" cmp -s "$SANDBOX/read" "$SANDBOX/small"
check "/a2.txt counts 2 links after rm /a.txt" test "$(nlink /a2.txt)" = 2

# symlinked TEXT PATH: ln -s TEXT PATH succeeds and reports PATH and a
# record of its own, which stat and ls report as a symbolic link whose
# target is TEXT, as given.
symlinked() {
    local text="$1" path="$2" dir="${2%/*}" made
    fs.ntfs "$img" ln -s "$text" "$path" >"$SANDBOX/ln.json" 2>"$SANDBOX/ln.err"
    local rc=$?
    check "ln -s $text $path exits 0 (got $rc: $(cat "$SANDBOX/ln.err"))" test "$rc" -eq 0
    check "ln -s $text $path prints nothing on stderr" test ! -s "$SANDBOX/ln.err"
    check "ln -s $text $path reports the path and a record: $(cat "$SANDBOX/ln.json")" \
        jq_is --arg p "$path" '.path == $p and .linked == true and (.inode | type) == "number" and length == 3' "$SANDBOX/ln.json"
    made="$(jq -r '.inode' "$SANDBOX/ln.json" 2>/dev/null)"
    fs.ntfs "$img" stat "$path" >"$SANDBOX/stat.json" 2>/dev/null
    check "stat $path is a symbolic link to $text in record $made: $(head -c 300 "$SANDBOX/stat.json")" \
        jq_is --arg t "$text" --arg r "$made" '.type == "symlink" and .target == $t and .inode == ($r | tonumber)' "$SANDBOX/stat.json"
    fs.ntfs "$img" ls "${dir:-/}" >"$SANDBOX/ls.json" 2>/dev/null
    check "ls ${dir:-/} lists ${path##*/} as a symbolic link to $text" \
        jq_is --arg n "${path##*/}" --arg t "$text" 'any(.[]; .name == $n and .type == "symlink" and .target == $t)' "$SANDBOX/ls.json"
}

symlinked a2.txt /rel.txt                        # relative, the same directory
symlinked ../big.bin /d/up.bin                   # relative, up a level
symlinked /d/f.txt /e/abs.txt                    # from the volume's root
symlinked 'C:\data\report.txt' /drive.txt        # a drive path
symlinked missing.txt /dangling.txt              # a target not there, as Windows allows

# A file takes names until its record has no room for another, and the
# first one it cannot hold is refused, writing nothing.
n=0
while [ "$n" -lt 100 ] && fs.ntfs "$img" ln /full.txt "/d/many-$n.txt" >/dev/null 2>&1; do
    n=$((n + 1))
done
check "/full.txt took names until its record was full ($n more)" test "$n" -gt 0 -a "$n" -lt 100
check "/full.txt counts every name it took" test "$(nlink /full.txt)" = $((n + 1))

fsck.ntfs "$img" >"$SANDBOX/fsck.json" 2>/dev/null
check "fsck.ntfs finds the volume clean after the links" test $? -eq 0

# --text, for people.
check "ln --text says what it linked" \
    test "$(fs.ntfs "$img" ln --text /a2.txt /t.txt 2>/dev/null)" = "linked /t.txt"

# --offset: the same volume 1 MiB into a whole-disk image, the first MiB
# left alone.
{ head -c 1048576 /dev/zero; cat "$img"; } >"$SANDBOX/whole.img"
fs.ntfs --offset 1048576 "$SANDBOX/whole.img" ln /a2.txt /d/off.txt >/dev/null 2>"$SANDBOX/off.err"
rc=$?
check "ln through --offset exits 0 (got $rc: $(cat "$SANDBOX/off.err"))" test "$rc" -eq 0
check "ln through --offset made the name inside the volume" \
    grep -qxF off.txt <<<"$(fs.ntfs --offset 1048576 "$SANDBOX/whole.img" ls /d 2>/dev/null | jq -r '.[].name')"
check "ln through --offset wrote nothing before the volume" \
    cmp -s -n 1048576 "$SANDBOX/whole.img" /dev/zero

# refused STATUS PATTERN ARG...: fs.ntfs IMG ln ARG... exits STATUS with
# nothing on stdout, a structured error matching PATTERN, and the image
# byte-identical.
refused() {
    local status="$1" pattern="$2"
    shift 2
    cp "$img" "$SANDBOX/before.img"
    fs.ntfs "$img" ln "$@" >"$SANDBOX/x.out" 2>"$SANDBOX/x.err"
    local rc=$?
    check "ln $* exits $status (got $rc: $(cat "$SANDBOX/x.err"))" test "$rc" -eq "$status"
    check "ln $* prints nothing on stdout" test ! -s "$SANDBOX/x.out"
    jq_check "ln $* says why" ".code == $status and (.error | test(\"$pattern\"; \"i\"))" "$SANDBOX/x.err"
    check "ln $* leaves the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"
}

# The name the record had no room for, asked again.
refused 1 'no room' /full.txt "/d/many-$n.txt"
# A name already there, as Windows compares names: through $UpCase, so
# case does not tell two names apart.
refused 1 'already exists' /a2.txt /d/a3.txt
refused 1 'already exists' /a2.txt /D/A3.TXT
refused 1 'already exists' -s x.txt /REL.TXT
refused 1 'already exists' -s x.txt /d
# NTFS gives a directory no second name.
refused 1 'hardlink directory' /d /d-link
refused 1 'hardlink directory' / /root-link
# What is not there, and a directory that is a file.
refused 1 'not found' /missing.txt /m.txt
refused 1 'not found' /a2.txt /nowhere/x.txt
refused 1 'not found' -s a2.txt /nowhere/x.txt
refused 1 'not a directory' /a2.txt /a2.txt/x
# Names Windows cannot address.
refused 1 'cannot address' /a2.txt '/a:b'
refused 1 'cannot address' -s x.txt '/a?b'
refused 1 'trailing space or period' /a2.txt /s.
refused 1 'NUL' -s x.txt /nul
refused 1 'name length' /a2.txt "/$(printf '%0256d' 0)"
# What is not a name to make: the root, `.`, `..`, and a metafile on
# either side.
refused 1 'root' /a2.txt /
refused 1 'cannot be made a link' /a2.txt /d/.
refused 1 'cannot be made a link' /a2.txt /d/..
# shellcheck disable=SC2016 # \$MFT and \$Extend are metafiles' names, not variables.
refused 1 'metafile' '/$MFT' /x
# shellcheck disable=SC2016
refused 1 'metafile' /a2.txt '/$Extend/x'
# A symbolic link names something, in no more than its record can hold.
refused 1 'empty' -s '' /empty.txt
refused 1 'no room' -s "$(printf '%01000d' 0)" /long.txt
# A trailing `/` says the name is a directory, and a link never is one.
refused 1 'not a directory' /a2.txt/ /x
refused 1 'not a directory' /a2.txt /x/
refused 1 'not a directory' -s a2.txt /x/

# A file at the 1024 links Windows allows takes no more. The count is set
# in /max.txt's record header (+0x12) by the on-disk layout.
poke "$img" $(($(record_offset "$img" "$(record /max.txt)") + 18)) '\000\004'
check "/max.txt's record counts 1024 links" \
    test "$(fs.ntfs "$img" stat /max.txt 2>/dev/null | jq -r '.ntfs.link_count')" = 1024
refused 1 '1024 links' /max.txt /max-2.txt

# The usage errors: exit 2, nothing on stdout.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" ln /a2.txt >"$SANDBOX/u.out" 2>/dev/null
check "ln without PATH is a usage error" test $? -eq 2
check "ln without PATH prints nothing on stdout" test ! -s "$SANDBOX/u.out"
fs.ntfs "$img" ln a2.txt /x >/dev/null 2>&1
check "a hard link to a path not from the root is a usage error" test $? -eq 2
fs.ntfs "$img" ln /a2.txt x >/dev/null 2>&1
check "ln to a path not from the root is a usage error" test $? -eq 2
fs.ntfs "$img" ln -s a2.txt x >/dev/null 2>&1
check "ln -s to a path not from the root is a usage error" test $? -eq 2
check "the usage errors leave the image byte-identical" cmp -s "$img" "$SANDBOX/before.img"

# A dirty volume asks for a check before anything is written to it.
fs.ntfs "$img" set dirty true >/dev/null 2>&1
check "set dirty true exits 0" test $? -eq 0
refused 1 'dirty' /a2.txt /dirty.txt
refused 1 'dirty' -s a2.txt /dirty.txt

check "ln --help carries an example" grep -q '^Examples:' <<<"$(fs.ntfs x ln --help 2>/dev/null)"

finish
