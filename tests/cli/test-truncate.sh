# File lengths through the installed CLI; Windows independently grades the
# resulting lengths, contents and zero tails in cli-truncate-win-verify-chkdsk.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

img="$SANDBOX/truncate.img"
mkfs.ntfs -q --size 64M "$img" >/dev/null 2>&1
check "the truncate image was formatted" test $? -eq 0
printf abcde >"$SANDBOX/resident"
head -c 12289 /dev/zero | tr '\000' Z >"$SANDBOX/nonresident"
for name in resident nonresident; do
    fs.ntfs "$img" write "/$name" <"$SANDBOX/$name" >/dev/null 2>&1
    check "$name was written" test $? -eq 0
done

resized() {
    local name="$1" size="$2" rc
    fs.ntfs "$img" truncate "/$name" "$size" >"$SANDBOX/out" 2>"$SANDBOX/err"
    rc=$?
    check "truncate /$name to $size exits 0 ($rc: $(cat "$SANDBOX/err"))" test "$rc" -eq 0
    check "successful truncate has no stderr" test ! -s "$SANDBOX/err"
    jq_check "truncate reports its path and numeric size" ". == {\"path\":\"/$name\",\"size\":$size}" "$SANDBOX/out"
    fs.ntfs "$img" read "/$name" >"$SANDBOX/back" 2>"$SANDBOX/err"
    check "read /$name succeeds" test $? -eq 0
    check "content and zero tail match" cmp -s "$SANDBOX/expected" "$SANDBOX/back"
    jq_check "stat sees the new length" ".size == $size" <(fs.ntfs "$img" stat "/$name")
    jq_check "the directory sees the new length" "any(.[]; .name == \"$name\" and .size == $size)" <(fs.ntfs "$img" ls /)
}
printf ab >"$SANDBOX/expected"
resized resident 2
head -c 8191 /dev/zero >>"$SANDBOX/expected"
resized resident 8193
head -c 4097 "$SANDBOX/nonresident" >"$SANDBOX/expected"
resized nonresident 4097
head -c 4096 /dev/zero >>"$SANDBOX/expected"
resized nonresident 8193
cp "$img" "$SANDBOX/same.img"
resized nonresident 8193
check "same-size truncate changes no bytes" cmp -s "$img" "$SANDBOX/same.img"
: >"$SANDBOX/expected"
resized nonresident 0
head -c 4097 /dev/zero >"$SANDBOX/expected"
resized nonresident 4097
fs.ntfs "$img" --text truncate /nonresident 4097 >"$SANDBOX/text" 2>"$SANDBOX/err"
check "text truncate exits 0" test $? -eq 0
check "text truncate names the path and size" grep -qE '/nonresident.*4097|4097.*/nonresident' "$SANDBOX/text"

fs.ntfs "$img" mkdir /dir >/dev/null 2>&1
check "refusal directory was made" test $? -eq 0
fs.ntfs "$img" ln -s /resident /link >/dev/null 2>&1
check "refusal symlink was made" test $? -eq 0
refused() {
    local code="$1" name="$2" size="$3" rc
    cp "$img" "$SANDBOX/before.img"
    fs.ntfs "$img" truncate "$name" "$size" >"$SANDBOX/out" 2>"$SANDBOX/err"
    rc=$?
    check "truncate $name $size exits $code (got $rc)" test "$rc" -eq "$code"
    check "refused truncate has no stdout" test ! -s "$SANDBOX/out"
    jq_check "refused truncate has a structured error" ".code == $code and (.error|type) == \"string\"" "$SANDBOX/err"
    check "refused truncate leaves every byte unchanged" cmp -s "$img" "$SANDBOX/before.img"
}
refused 1 /dir 0
refused 3 /link 0
refused 1 /missing 0
refused 1 '/$MFT' 0
refused 1 /resident/ 0
refused 1 /nonresident 18446744073709551615
for size in nope 1K -1 1.5 18446744073709551616; do refused 2 /resident "$size"; done
fs.ntfs "$SANDBOX/absent.img" truncate /f nope >"$SANDBOX/out" 2>"$SANDBOX/err"
check "bad size is rejected before opening an absent target" test $? -eq 2
check "bad size never creates the target" test ! -e "$SANDBOX/absent.img"

# Attribute flags at +12 in Microsoft's ATTRIBUTE_RECORD_HEADER. Change
# only this word, away from sector fixups, to exercise each unsupported shape.
record="$(fs.ntfs "$img" stat /resident | jq -r .ntfs.record_number)"
base="$(record_offset "$img" "$record")"
attr=$((base + $(u16 "$img" $((base + 20)))))
while [ "$(od -An -tu4 -j "$attr" -N4 "$img" | tr -d ' \n')" != 128 ]; do
    length="$(od -An -tu4 -j $((attr + 4)) -N4 "$img" | tr -d ' \n')"
    if [ "${length:-0}" -lt 24 ]; then fail 'cannot locate unnamed DATA flags'; finish; fi
    attr=$((attr + length))
done
for flags in '\001\000' '\000\100' '\000\200'; do
    poke "$img" $((attr + 12)) "$flags"
    refused 3 /resident 0
    # Restore only this test's flag word; no image or test is deleted.
    poke "$img" $((attr + 12)) '\000\000'
done
fs.ntfs "$img" set dirty true >/dev/null 2>&1
check "dirty refusal fixture was prepared" test $? -eq 0
refused 1 /resident 1
finish
