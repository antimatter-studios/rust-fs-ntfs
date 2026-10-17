# Windows enumerates a tree it made, opening each item without following
# reparses for its type, size and file ID. The fixture producer records
# that independent walk beside the image; missing evidence fails.
source "$(dirname "$0")/../cli/lib.sh"
img="$REPO/test-disks/ntfs-ls-recursive.img"
oracle="$REPO/test-disks/ntfs-ls-recursive.windows.tsv"
for file in "$img" "$oracle"; do
    if [ ! -s "$file" ]; then
        fail "$file missing: run test-disks/build-windows-native-read-fixtures.ps1 on Windows (CI: windows-native-read-fixtures)"
        finish
    fi
done
cp "$img" "$SANDBOX/before.img"
fs.ntfs "$img" ls -R / >"$SANDBOX/tree.json" 2>"$SANDBOX/error"
check "the Windows tree lists successfully ($(cat "$SANDBOX/error"))" test $? -eq 0
count=0
while IFS=$'\t' read -r path kind size record depth; do
    count=$((count + 1))
    jq_check "$path has Windows' type, size, record and depth" \
        "[.[] | select(.path == \"$path\") | [.type,.size,.record,.depth]] == [[\"$kind\",$size,$record,$depth]]" "$SANDBOX/tree.json"
done <"$oracle"
check "Windows enumerated the entire fixture (got $count)" test "$count" -ge 15
check "no path was lost, invented or entered through a reparse" \
    test "$(jq length "$SANDBOX/tree.json")" -eq "$count"
# UTF-8 path order equals depth-first byte order, since the parent precedes
# its descendants in this fixture and no sibling begins with a parent's name.
LC_ALL=C cut -f1 "$oracle" | LC_ALL=C sort >"$SANDBOX/want"
jq -r '.[].path' "$SANDBOX/tree.json" >"$SANDBOX/got"
check "the walk is byte-sorted depth first" cmp -s "$SANDBOX/want" "$SANDBOX/got"
jq_check "both hard-link names appear with the same identity" \
    '[.[] | select(.path == "/a/hard" or .path == "/z") | .record] | length == 2 and .[0] == .[1]' "$SANDBOX/tree.json"
for link in /dir-link /junction /a/back /dangling; do
    fs.ntfs "$img" ls -R "$link" >"$SANDBOX/link.json" 2>/dev/null
    check "a reparse starting point $link succeeds" test $? -eq 0
    jq_check "$link alone is listed at depth 0" 'length == 1 and .[0].depth == 0 and (.[0].type == "symlink" or .[0].type == "junction")' "$SANDBOX/link.json"
done
fs.ntfs "$img" ls -R --max-depth 0 --limit 0 /empty >"$SANDBOX/empty.json" 2>/dev/null
check "an empty tree fits zero bounds" test $? -eq 0
jq_check "an empty tree lists nothing" '. == []' "$SANDBOX/empty.json"
fs.ntfs "$img" ls -R --all --limit 1 / >"$SANDBOX/all.json" 2>/dev/null
check "--all also obeys the entry bound" test $? -eq 1
jq_check "--all includes metafiles and never dot entries" 'any(.[]; .record < 16) and all(.[]; .name != "." and .name != "..")' "$SANDBOX/all.json"
check "every read leaves the Windows volume byte-identical" cmp -s "$img" "$SANDBOX/before.img"
finish
