# Positioned writes through the installed CLI. The Windows tier independently
# repeats the writes through FileStream and grades the resulting image.
source "$(dirname "$0")/lib.sh"
source "$(dirname "$0")/lib-ntfs.sh"

img="$SANDBOX/write-at.img"
mkfs.ntfs -q --size 64M "$img" >/dev/null 2>&1
check "the positioned-write image was formatted" test $? -eq 0
printf abcde >"$SANDBOX/resident"
head -c 12289 /dev/zero | tr '\000' Z >"$SANDBOX/nonresident"
: >"$SANDBOX/empty"
printf 'A\000C' >"$SANDBOX/payload"
: >"$SANDBOX/nothing"
for name in resident nonresident empty; do
    fs.ntfs "$img" write "/$name" <"$SANDBOX/$name" >/dev/null 2>&1
    check "$name was written" test $? -eq 0
done

positioned() {
    local name="$1" at="$2" input="$3" bytes size rc
    bytes="$(wc -c <"$input" | tr -d ' ')"
    if [ "$bytes" -gt 0 ]; then
        dd if="$input" of="$SANDBOX/$name" bs=1 seek="$at" conv=notrunc 2>/dev/null
    fi
    size="$(wc -c <"$SANDBOX/$name" | tr -d ' ')"
    fs.ntfs "$img" write --at "$at" "/$name" <"$input" >"$SANDBOX/out" 2>"$SANDBOX/err"
    rc=$?
    check "write --at $at /$name exits 0 ($rc: $(cat "$SANDBOX/err"))" test "$rc" -eq 0
    check "successful positioned write has no stderr" test ! -s "$SANDBOX/err"
    jq_check "position and byte counts are numeric" ". == {\"path\":\"/$name\",\"at\":$at,\"bytes\":$bytes,\"size\":$size}" "$SANDBOX/out"
    fs.ntfs "$img" read "/$name" >"$SANDBOX/back" 2>"$SANDBOX/err"
    check "read /$name succeeds" test $? -eq 0
    check "prefix, patch, suffix and any zero gap match" cmp -s "$SANDBOX/$name" "$SANDBOX/back"
    jq_check "stat sees the resulting length" ".size == $size" <(fs.ntfs "$img" stat "/$name")
    jq_check "the directory sees the resulting length" "any(.[]; .name == \"$name\" and .size == $size)" <(fs.ntfs "$img" ls /)
    check "positioned write releases a clean volume" test "$(fs.ntfs "$img" get dirty --text)" = false
}
positioned resident 2 "$SANDBOX/payload"
positioned resident 20 "$SANDBOX/payload"
positioned resident 4095 "$SANDBOX/payload"
positioned nonresident 20 "$SANDBOX/payload"
positioned nonresident 4095 "$SANDBOX/payload"
positioned nonresident 12289 "$SANDBOX/payload"
positioned nonresident 16385 "$SANDBOX/payload"
positioned empty 0 "$SANDBOX/payload"
cp "$img" "$SANDBOX/before.img"
positioned nonresident 18446744073709551615 "$SANDBOX/nothing"
check "empty input beyond EOF changes no image bytes" cmp -s "$img" "$SANDBOX/before.img"
fs.ntfs "$img" --text write --at 1 /resident <"$SANDBOX/payload" >"$SANDBOX/text" 2>"$SANDBOX/err"
check "text positioned write succeeds" test $? -eq 0
check "text report names path and position" grep -qE '/resident.*1|1.*/resident' "$SANDBOX/text"

fs.ntfs "$img" mkdir /dir >/dev/null 2>&1
check "refusal directory was made" test $? -eq 0
fs.ntfs "$img" ln -s /resident /link >/dev/null 2>&1
check "refusal symlink was made" test $? -eq 0
refused() {
    local code="$1" name="$2" at="$3" rc
    cp "$img" "$SANDBOX/before.img"
    fs.ntfs "$img" write --at "$at" "$name" <"$SANDBOX/payload" >"$SANDBOX/out" 2>"$SANDBOX/err"
    rc=$?
    check "write --at $at $name exits $code (got $rc)" test "$rc" -eq "$code"
    check "refused write has no stdout" test ! -s "$SANDBOX/out"
    jq_check "refused write has a structured error" ".code == $code and (.error|type) == \"string\"" "$SANDBOX/err"
    check "refused write leaves every image byte unchanged" cmp -s "$img" "$SANDBOX/before.img"
}
refused 4 /missing 0
refused 1 /dir 0
refused 3 /link 0
refused 1 '/$MFT' 0
refused 1 /resident/ 0
refused 2 relative 0
refused 1 /resident 18446744073709551615
refused 1 /resident 67108864
for at in nope 1K -1 1.5 18446744073709551616; do refused 2 /resident "$at"; done

# Attribute flags at +12 in Microsoft's ATTRIBUTE_RECORD_HEADER. The
# library must not splice even resident data marked transformed.
record="$(fs.ntfs "$img" stat /empty | jq -r .ntfs.record_number)"
base="$(record_offset "$img" "$record")"
attr=$((base + $(u16 "$img" $((base + 20)))))
# The first attribute is STANDARD_INFORMATION; its file attributes word
# is at +32 of the resident value. Windows denies write access to READONLY.
readonly=$((attr + $(u16 "$img" $((attr + 20))) + 32))
was="$(byte_at "$img" "$readonly")"
poke "$img" "$readonly" "$(printf '\\x%02x' $((0x$was | 1)))"
refused 1 /empty 0
poke "$img" "$readonly" "$(printf '\\x%s' "$was")"
while [ "$(od -An -tu4 -j "$attr" -N4 "$img" | tr -d ' \n')" != 128 ]; do
    length="$(od -An -tu4 -j $((attr + 4)) -N4 "$img" | tr -d ' \n')"
    if [ "${length:-0}" -lt 24 ]; then fail 'cannot locate unnamed DATA flags'; finish; fi
    attr=$((attr + length))
done
for flags in '\001\000' '\000\100' '\000\200'; do
    poke "$img" $((attr + 12)) "$flags"
    refused 3 /empty 0
    poke "$img" $((attr + 12)) '\000\000'
done

# An actual stdin read error must happen before any image open or edit.
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" write --at 0 /resident <"$SANDBOX" >"$SANDBOX/out" 2>"$SANDBOX/err"
check "an input read error fails" test $? -eq 1
check "an input read error emits no stdout" test ! -s "$SANDBOX/out"
jq_check "stdin error is named" '.code == 1 and (.error|contains("read stdin"))' "$SANDBOX/err"
check "input failure changes no image bytes" cmp -s "$img" "$SANDBOX/before.img"

head -c 1048576 /dev/zero >"$SANDBOX/prefix"
cat "$SANDBOX/prefix" "$img" >"$SANDBOX/disk.img"
fs.ntfs --offset 1048576 "$SANDBOX/disk.img" write --at 1 /empty <"$SANDBOX/payload" >"$SANDBOX/out" 2>"$SANDBOX/err"
check "device and file offsets work together" test $? -eq 0
jq_check "the file offset remains distinct" '.at == 1 and .bytes == 3 and .size == 4' "$SANDBOX/out"
fs.ntfs --offset 1048576 "$SANDBOX/disk.img" read /empty >"$SANDBOX/back" 2>"$SANDBOX/err"
check "partition read succeeds" test $? -eq 0
printf 'AA\000C' >"$SANDBOX/expected"
check "partition patch has the expected bytes" cmp -s "$SANDBOX/expected" "$SANDBOX/back"
check "whole-disk prefix remains unchanged" cmp -s "$SANDBOX/prefix" <(head -c 1048576 "$SANDBOX/disk.img")
fsck.ntfs "$img" >/dev/null 2>"$SANDBOX/err"
check "the patched volume is clean" test $? -eq 0
fs.ntfs "$img" set dirty true >/dev/null 2>&1
check "dirty refusal fixture was prepared" test $? -eq 0
refused 1 /resident 0
finish
