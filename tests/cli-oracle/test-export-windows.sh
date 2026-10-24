# Windows writes and hashes every source; the shared transfer publishes it.
source "$(dirname "$0")/../cli/lib.sh"

img="$REPO/test-disks/ntfs-cli-read.img"
oracle="$REPO/test-disks/ntfs-cli-read.sha256.tsv"
compressed="$REPO/test-disks/ntfs-compressed.img"
compressed_oracle="$REPO/test-disks/ntfs-compressed.sha256.tsv"
for file in "$img" "$oracle" "$REPO/test-disks/ntfs-symlink.img" "$compressed" "$compressed_oracle"; do
    if [ ! -s "$file" ]; then
        fail "$file missing: run test-disks/build-windows-native-read-fixtures.ps1 (CI: windows-native-read-fixtures)"
        finish
    fi
done
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}
before="$(sha256 <"$img")"
rows=0
while IFS="$(printf '\t')" read -r item size hash; do
    rows=$((rows + 1))
    dest="$SANDBOX/file-$rows"
    fs.ntfs "$img" export "$item" "$dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    status=$?
    check "export $item succeeds ($(cat "$SANDBOX/err"))" test "$status" -eq 0
    check "export $item publishes Windows' size" test "$(wc -c <"$dest")" -eq "$size"
    check "export $item matches Windows SHA-256" test "$(sha256 <"$dest")" = "$hash"
    jq_check "export reports Windows' byte count and digest" \
        ".bytes == $size and .sha256 == \"$hash\" and .replaced == false" "$SANDBOX/out"
    fs.ntfs "$img" export "$item" - >"$SANDBOX/stdout" 2>"$SANDBOX/err"
    check "stdout export succeeds" test $? -eq 0
    check "stdout contains only Windows' bytes" test "$(sha256 <"$SANDBOX/stdout")" = "$hash"
    printf sentinel >"$dest"
    fs.ntfs "$img" export "$item" "$dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "existing destination needs --replace" test $? -eq 1
    check "refusal preserves the destination" test "$(cat "$dest")" = sentinel
    fs.ntfs "$img" export "$item" "$dest" --replace >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "explicit replacement succeeds" test $? -eq 0
    check "replacement has Windows' bytes" test "$(sha256 <"$dest")" = "$hash"
    jq_check "replacement is reported" '.replaced == true' "$SANDBOX/out"
done <"$oracle"
check "all eight Windows files were exported" test "$rows" -eq 8
check "empty and multi-chunk boundary fixtures are present" \
    awk -F '\t' '$1 == "/empty.bin" && $2 == 0 { empty=1 } $1 == "/random.bin" && $2 == 3145729 { large=1 } END { exit !(empty && large) }' "$oracle"

dd if=/dev/zero of="$SANDBOX/disk.img" bs=4096 count=1 2>/dev/null
cat "$img" >>"$SANDBOX/disk.img"
fs.ntfs "$SANDBOX/disk.img" export --offset 4096 /random.bin "$SANDBOX/offset" >"$SANDBOX/out" 2>"$SANDBOX/err"
check "export honors the partition offset" test $? -eq 0
check "offset export matches Windows' digest" test "$(sha256 <"$SANDBOX/offset")" = "$(awk -F '\t' '$1 == "/random.bin" { print $3 }' "$oracle")"

for item in / /sub /missing; do
    fs.ntfs "$img" export "$item" "$SANDBOX/refused" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "$item is refused" test $? -eq 1
    check "refusal publishes nothing" test ! -e "$SANDBOX/refused"
    check "refusal has no stdout" test ! -s "$SANDBOX/out"
done
for fixture in 'ntfs-symlink.img /rel-file'; do
    read -r source item <<<"$fixture"
    fs.ntfs "$REPO/test-disks/$source" export "$item" "$SANDBOX/refused" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "export refuses transformed or linked data" test $? -ne 0
    check "refused transformed data publishes nothing" test ! -e "$SANDBOX/refused"
done

# LZNT1 streams Windows compressed export one compression unit at a time:
# mixed.bin holds compressed, deallocated and raw-stored units and a
# partial last unit, each graded against the digest Windows computed.
compressed_rows=0
while IFS="$(printf '\t')" read -r item size hash; do
    compressed_rows=$((compressed_rows + 1))
    dest="$SANDBOX/compressed-$compressed_rows"
    fs.ntfs "$compressed" export "$item" "$dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    status=$?
    check "compressed export $item succeeds ($(cat "$SANDBOX/err"))" test "$status" -eq 0
    check "compressed export $item publishes Windows' size" test "$(wc -c <"$dest")" -eq "$size"
    check "compressed export $item matches Windows SHA-256" test "$(sha256 <"$dest")" = "$hash"
    jq_check "compressed export reports Windows' byte count and digest" \
        ".bytes == $size and .sha256 == \"$hash\"" "$SANDBOX/out"
    fs.ntfs "$compressed" export "$item" - >"$SANDBOX/stdout" 2>"$SANDBOX/err"
    check "compressed stdout export has Windows' bytes" test "$(sha256 <"$SANDBOX/stdout")" = "$hash"
done <"$compressed_oracle"
check "both Windows-compressed files were exported" test "$compressed_rows" -eq 2
check "the mixed fixture spans more than six compression units" \
    awk -F '\t' '$1 == "/mixed.bin" && $2 > 6 * 65536 { found=1 } END { exit !found }' "$compressed_oracle"
fs.ntfs "$img" export /empty.bin - --replace >"$SANDBOX/out" 2>"$SANDBOX/err"
check "--replace with stdout is a usage error" test $? -eq 2
fs.ntfs "$img" export /empty.bin >"$SANDBOX/out" 2>"$SANDBOX/err"
check "export requires DEST" test $? -eq 2
check "export help has an example" grep -q '^Examples:' <<<"$(fs.ntfs "$img" export --help)"

# Damage only the Windows file's mapping pairs, sending its data beyond the
# image. BPB/MFT/attribute offsets follow the published NTFS format (see
# docs/provenance.md), not another implementation's source. Metadata remains
# readable, so the failure exercises source data I/O and staging cleanup.
fs.ntfs "$img" stat /random.bin >"$SANDBOX/stat" 2>"$SANDBOX/err"
check "fault target metadata is readable" test $? -eq 0
cp "$img" "$SANDBOX/damaged.img"
python3 - "$SANDBOX/damaged.img" "$(jq -r .inode "$SANDBOX/stat")" <<'PY'
import os
import struct
import sys

with open(sys.argv[1], 'r+b') as image:
    boot = image.read(512)
    sector = struct.unpack_from('<H', boot, 11)[0]
    cluster = sector * boot[13]
    encoded = struct.unpack_from('<b', boot, 64)[0]
    size = (1 << -encoded) if encoded < 0 else encoded * cluster
    mft = struct.unpack_from('<Q', boot, 48)[0] * cluster
    at = mft + int(sys.argv[2]) * size
    image.seek(at)
    record = bytearray(image.read(size))
    assert record[:4] == b'FILE', 'fixture target is outside the initial MFT extent'
    usa, count = struct.unpack_from('<HH', record, 4)
    for i in range(1, count):
        record[i * sector - 2:i * sector] = record[usa + i * 2:usa + i * 2 + 2]
    attr = struct.unpack_from('<H', record, 20)[0]
    while struct.unpack_from('<I', record, attr)[0] != 0xFFFFFFFF:
        kind, length = struct.unpack_from('<II', record, attr)
        assert length >= 24
        if kind == 0x80 and record[attr + 9] == 0:
            assert record[attr + 8] == 1, 'fault fixture must be non-resident'
            runs = attr + struct.unpack_from('<H', record, attr + 32)[0]
            last = struct.unpack_from('<Q', record, attr + 24)[0]
            n = (last + 1).to_bytes(max(1, ((last + 1).bit_length() + 7) // 8), 'little')
            outside = os.fstat(image.fileno()).st_size // cluster + 1
            lcn = outside.to_bytes((outside.bit_length() + 8) // 8, 'little', signed=True)
            pairs = bytes([(len(lcn) << 4) | len(n)]) + n + lcn + b'\0'
            assert runs + len(pairs) <= attr + length, 'fixture mapping-pair space is too small'
            record[runs:runs + len(pairs)] = pairs
            break
        attr += length
    else:
        raise AssertionError('fixture has no unnamed DATA')
    for i in range(1, count):
        record[usa + i * 2:usa + i * 2 + 2] = record[i * sector - 2:i * sector]
        record[i * sector - 2:i * sector] = record[usa:usa + 2]
    image.seek(at)
    image.write(record)
PY
check "unreadable data mapping was installed" test $? -eq 0
for replace in no yes; do
    dest="$SANDBOX/damaged-$replace"
    args=()
    if [ "$replace" = yes ]; then printf sentinel >"$dest"; args=(--replace); fi
    fs.ntfs "$SANDBOX/damaged.img" export /random.bin "$dest" "${args[@]}" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "damaged data fails export" test $? -eq 1
    jq_check "damaged data uses the shared failure contract" '.code == 1' "$SANDBOX/err"
    check "damaged export has no success report" test ! -s "$SANDBOX/out"
    if [ "$replace" = yes ]; then
        check "failed replacement preserves old bytes" test "$(cat "$dest")" = sentinel
    else
        check "failed export publishes no destination" test ! -e "$dest"
    fi
done
check "failed exports remove staging files" test -z "$(find "$SANDBOX" -name '*.partial' -print)"
check "source remains byte-identical" test "$(sha256 <"$img")" = "$before"
finish
