# Native NTFS EAs are metadata; one selected value is raw content.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/xattr.img"
mkfs.ntfs -q --size 64M "$img" >/dev/null 2>&1
check "mkfs made the image" test $? -eq 0
printf content | fs.ntfs "$img" write /plain >/dev/null 2>&1
check "write made a file" test $? -eq 0
fs.ntfs "$img" mkdir /dir >/dev/null 2>&1
check "mkdir made a directory" test $? -eq 0
cp "$img" "$SANDBOX/before.img"
for item in /plain /PLAIN /dir /; do
    fs.ntfs "$img" xattr "$item" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "xattr $item succeeds" test $? -eq 0
    jq_check "xattr $item has no EAs" '. == []' "$SANDBOX/out"
    check "xattr $item has no stderr" test ! -s "$SANDBOX/err"
done
fs.ntfs "$img" xattr --text /plain >"$SANDBOX/out" 2>"$SANDBOX/err"
check "text listing succeeds" test $? -eq 0
check "empty text listing has no stderr" test ! -s "$SANDBOX/err"
for item in /plain /missing; do
    printf kept >"$SANDBOX/dest"
    fs.ntfs "$img" xattr "$item" user.absent -o "$SANDBOX/dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "missing EA or path fails" test $? -eq 1
    check "refusal emits no stdout" test ! -s "$SANDBOX/out"
    jq_check "missing EA or path has a structured error" '.code == 1 and (.error|contains("not found"))' "$SANDBOX/err"
    check "refusal preserves the destination" test "$(cat "$SANDBOX/dest")" = kept
    check "refusal leaves no partial file" test ! -e "$SANDBOX/dest.partial"
done
fs.ntfs "$img" xattr /plain user.absent >"$SANDBOX/out" 2>"$SANDBOX/err"
check "missing name fails" test $? -eq 1
jq_check "missing name is named in the error" '.error|contains("user.absent")' "$SANDBOX/err"
fs.ntfs "$img" xattr /missing >"$SANDBOX/out" 2>"$SANDBOX/err"
check "missing listing path fails" test $? -eq 1
check "missing listing path has no stdout" test ! -s "$SANDBOX/out"
for args in '' '/plain -o dest' '/plain user.a extra'; do
    # Intentional word splitting supplies each wrong command line.
    fs.ntfs "$img" xattr $args >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "wrong arguments fail with usage" test $? -eq 2
    check "wrong arguments emit no stdout" test ! -s "$SANDBOX/out"
done
dd if=/dev/zero of="$SANDBOX/disk.img" bs=4096 count=1 2>/dev/null
cat "$img" >>"$SANDBOX/disk.img"
cp "$SANDBOX/disk.img" "$SANDBOX/disk-before.img"
fs.ntfs "$SANDBOX/disk.img" xattr --offset 4096 /plain >"$SANDBOX/out" 2>"$SANDBOX/err"
check "xattr honors the device offset" test $? -eq 0
jq_check "offset listing is empty" '. == []' "$SANDBOX/out"
check "all reads and refusals preserve the image" cmp -s "$img" "$SANDBOX/before.img"
check "offset reads preserve the disk" cmp -s "$SANDBOX/disk.img" "$SANDBOX/disk-before.img"
finish
