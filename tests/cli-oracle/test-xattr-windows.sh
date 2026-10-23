# NtQueryEaFile supplies stored names, lengths, flags and every value byte.
source "$(dirname "$0")/../cli/lib.sh"
img="$REPO/test-disks/ntfs-ea.img"
oracle="$REPO/test-disks/ntfs-ea.query.tsv"
for file in "$img" "$oracle"; do
    if [ ! -s "$file" ]; then
        fail "$file missing: run test-disks/build-windows-native-read-fixtures.ps1 on Windows (CI: windows-native-read-fixtures)"
        finish
    fi
done
cp "$img" "$SANDBOX/before.img"
rows=0
cut -f1 "$oracle" | LC_ALL=C sort -u >"$SANDBOX/paths"
while IFS= read -r item; do
    fs.ntfs "$img" xattr "$item" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "Windows EA listing succeeds for $item" test $? -eq 0
    jq -r '.[] | [.name,.size,(if .ntfs.need_ea then 128 else 0 end)] | @tsv' "$SANDBOX/out" | LC_ALL=C sort >"$SANDBOX/got"
    awk -F '\t' -v p="$item" '$1 == p { print $2 "\t" $3 "\t" $4 }' "$oracle" | LC_ALL=C sort >"$SANDBOX/want"
    check "stored names, sizes and NEED_EA equal NtQueryEaFile" cmp -s "$SANDBOX/want" "$SANDBOX/got"
done <"$SANDBOX/paths"
while IFS="$(printf '\t')" read -r item name size flags hex; do
    printf '%s' "$hex" | python3 -c 'import sys; sys.stdout.buffer.write(bytes.fromhex(sys.stdin.read()))' >"$SANDBOX/want"
    fs.ntfs "$img" xattr "$item" "$name" >"$SANDBOX/read" 2>"$SANDBOX/err"
    check "Windows EA $name reads successfully" test $? -eq 0
    check "EA bytes equal Windows' query" cmp -s "$SANDBOX/want" "$SANDBOX/read"
    check "EA length equals Windows' query" test "$(wc -c <"$SANDBOX/read")" -eq "$size"
    lower="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]')"
    fs.ntfs "$img" xattr "$item" "$lower" -o "$SANDBOX/dest" >"$SANDBOX/out" 2>"$SANDBOX/err"
    check "EA lookup ignores ASCII case and supports -o" test $? -eq 0
    check "-o writes exactly Windows' bytes" cmp -s "$SANDBOX/want" "$SANDBOX/dest"
    check "-o emits no stdout" test ! -s "$SANDBOX/out"
    check "-o leaves no partial file" test ! -e "$SANDBOX/dest.partial"
    rows=$((rows + 1))
done <"$oracle"
check "all 18 Windows EAs were checked" test "$rows" -eq 18
check "EA inspection leaves the image unchanged" cmp -s "$img" "$SANDBOX/before.img"
finish
