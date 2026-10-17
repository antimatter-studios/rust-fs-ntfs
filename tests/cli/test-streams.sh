# Named stream enumeration is read-only; plain read still selects unnamed data.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/streams.img"
mkfs.ntfs -q --size 64M "$img" >/dev/null 2>&1
check "mkfs made the image" test $? -eq 0
printf 'unnamed bytes\n' >"$SANDBOX/source"
fs.ntfs "$img" write /plain <"$SANDBOX/source" >/dev/null 2>&1
check "write made an ordinary file" test $? -eq 0
fs.ntfs "$img" mkdir /dir >/dev/null 2>&1
check "mkdir made a directory" test $? -eq 0
cp "$img" "$SANDBOX/before.img"

for item in /plain /dir /; do
    fs.ntfs "$img" streams "$item" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "streams $item exits 0" test $? -eq 0
    jq_check "streams $item excludes unnamed data and other attribute types" '. == []' "$SANDBOX/out"
    check "streams $item has no stderr" test ! -s "$SANDBOX/err"
done
fs.ntfs "$img" streams --text /plain >"$SANDBOX/text" 2>"$SANDBOX/err"
check "streams --text exits 0" test $? -eq 0
check "streams --text has no stderr" test ! -s "$SANDBOX/err"
fs.ntfs "$img" streams /PLAIN >"$SANDBOX/out" 2>/dev/null
check "streams uses Windows case-insensitive path lookup" test $? -eq 0
jq_check "case-insensitive enumeration is empty" '. == []' "$SANDBOX/out"
fs.ntfs "$img" read /plain >"$SANDBOX/read" 2>/dev/null
check "plain read still exits 0" test $? -eq 0
check "plain read still returns unnamed bytes" cmp -s "$SANDBOX/source" "$SANDBOX/read"

for item in /missing /plain:Zone.Identifier; do
    fs.ntfs "$img" streams "$item" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "streams $item fails without interpreting a stream suffix" test $? -eq 1
    check "streams $item failure has no stdout" test ! -s "$SANDBOX/out"
    jq_check "streams $item uses the shared error contract" '.code == 1 and (.error | test("not found"))' "$SANDBOX/err"
done
fs.ntfs "$img" streams >"$SANDBOX/out" 2>"$SANDBOX/err"
check "streams requires a path" test $? -eq 2
check "missing path argument produces no stdout" test ! -s "$SANDBOX/out"

# --offset is the device offset, including when supplied after the verb.
dd if=/dev/zero of="$SANDBOX/disk.img" bs=4096 count=1 2>/dev/null
cat "$img" >>"$SANDBOX/disk.img"
cp "$SANDBOX/disk.img" "$SANDBOX/disk-before.img"
fs.ntfs "$SANDBOX/disk.img" streams --offset 4096 /plain >"$SANDBOX/out" 2>"$SANDBOX/err"
check "streams honors the filesystem offset" test $? -eq 0
jq_check "offset enumeration excludes unnamed data" '. == []' "$SANDBOX/out"
check "enumeration and refusals leave the image unchanged" cmp -s "$img" "$SANDBOX/before.img"
check "offset enumeration leaves the whole disk unchanged" cmp -s "$SANDBOX/disk.img" "$SANDBOX/disk-before.img"

# ntfscp supplies streams independently of this driver's writer. The Windows
# oracle tier separately compares reads with Get-Content -Stream captures.
command -v ntfscp >/dev/null 2>&1 || {
    fail "ntfscp missing: install ntfs-3g (apt-get install ntfs-3g)"
    finish
}
printf 'named\000bytes\377' >"$SANDBOX/named"
head -c 2097169 /dev/zero | tr '\000' Z >"$SANDBOX/large"
: >"$SANDBOX/empty"
for name in named large empty; do
    ntfscp -N "$name" "$img" "$SANDBOX/$name" /plain >"$SANDBOX/setup" 2>&1
    rc=$?
    check "ntfscp authored $name ($(cat "$SANDBOX/setup"))" test "$rc" -eq 0
done
fs.ntfs "$img" ln -s /plain /link >/dev/null 2>&1
check "the refusal link was made" test $? -eq 0
cp "$img" "$SANDBOX/named-before.img"
for name in named large empty; do
    fs.ntfs "$img" read --stream "$name" /plain >"$SANDBOX/read" 2>"$SANDBOX/err"
    check "read --stream $name exits 0" test $? -eq 0
    check "read --stream $name matches, including multiple 1 MiB chunks" cmp -s "$SANDBOX/$name" "$SANDBOX/read"
    check "successful named read has no stderr" test ! -s "$SANDBOX/err"
    fs.ntfs "$img" read --stream "$name" /plain -o "$SANDBOX/dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "read --stream $name -o exits 0" test $? -eq 0
    check "named -o returns no stdout" test ! -s "$SANDBOX/out"
    check "named -o contains exactly the stream" cmp -s "$SANDBOX/$name" "$SANDBOX/dest"
    check "named -o leaves no partial file" test ! -e "$SANDBOX/dest.partial"
done
fs.ntfs "$img" read --stream NAMED /PLAIN >"$SANDBOX/read" 2>"$SANDBOX/err"
check "stream and path lookup follow Windows case folding" test $? -eq 0
check "case-folded selection returns the named bytes" cmp -s "$SANDBOX/named" "$SANDBOX/read"
fs.ntfs "$img" read /plain >"$SANDBOX/read" 2>"$SANDBOX/err"
check "unnamed read remains compatible after streams are added" test $? -eq 0
check "unnamed bytes remain separate" cmp -s "$SANDBOX/source" "$SANDBOX/read"
for item in /plain /dir /link /plain:named; do
    printf kept >"$SANDBOX/dest"
    cp "$SANDBOX/dest" "$SANDBOX/kept"
    fs.ntfs "$img" read --stream absent "$item" -o "$SANDBOX/dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "named read refuses $item before output" test $? -eq 1
    check "refused named read has no stdout" test ! -s "$SANDBOX/out"
    jq_check "refusal is structured" '.code == 1 and (.error|type) == "string"' "$SANDBOX/err"
    check "refusal preserves an existing destination" cmp -s "$SANDBOX/kept" "$SANDBOX/dest"
    check "refusal creates no partial file" test ! -e "$SANDBOX/dest.partial"
done
fs.ntfs "$img" read --stream absent /plain >"$SANDBOX/out" 2>"$SANDBOX/err"
check "a missing stream exits 1" test $? -eq 1
check "missing stream has no stdout" test ! -s "$SANDBOX/out"
jq_check "missing stream error names the stream" '.code == 1 and (.error|contains("absent")) and (.error|contains("not found"))' "$SANDBOX/err"
fs.ntfs "$img" read --stream named /link >"$SANDBOX/out" 2>"$SANDBOX/err"
check "an existing stream on the link target is not followed" test $? -eq 1
check "link refusal emits no bytes" test ! -s "$SANDBOX/out"
jq_check "link refusal keeps the existing read policy" '.error|contains("is a link to")' "$SANDBOX/err"
dd if=/dev/zero of="$SANDBOX/named-disk.img" bs=4096 count=1 2>/dev/null
cat "$img" >>"$SANDBOX/named-disk.img"
cp "$SANDBOX/named-disk.img" "$SANDBOX/named-disk-before.img"
fs.ntfs "$SANDBOX/named-disk.img" read --offset 4096 --stream large /plain >"$SANDBOX/read" 2>"$SANDBOX/err"
check "named read honors the device offset" test $? -eq 0
check "offset read returns the selected stream" cmp -s "$SANDBOX/large" "$SANDBOX/read"
check "all named reads and refusals preserve the image" cmp -s "$img" "$SANDBOX/named-before.img"
check "offset named read preserves the disk" cmp -s "$SANDBOX/named-disk.img" "$SANDBOX/named-disk-before.img"
finish
