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
finish
