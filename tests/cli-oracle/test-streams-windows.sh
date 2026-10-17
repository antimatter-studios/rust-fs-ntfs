# Windows authors the streams and reports their names, lengths and allocation.
# Includes extension records, empty, Unicode, non-resident and directory streams.
source "$(dirname "$0")/../cli/lib.sh"
rows=0
for fixture in ntfs-attrlist ntfs-stat; do
    img="$REPO/test-disks/$fixture.img"
    oracle="$REPO/test-disks/$fixture.streams.tsv"
    for file in "$img" "$oracle"; do
        if [ ! -s "$file" ]; then
            fail "$file missing: run test-disks/build-windows-native-read-fixtures.ps1 on Windows (CI: windows-native-read-fixtures)"
            finish
        fi
    done
    cp "$img" "$SANDBOX/before.img"
    cut -f1 "$oracle" | LC_ALL=C sort -u >"$SANDBOX/paths"
    while IFS= read -r item; do
        fs.ntfs "$img" streams "$item" >"$SANDBOX/out" 2>"$SANDBOX/err"
        rc=$?
        check "streams $fixture $item succeeds ($(cat "$SANDBOX/err"))" test "$rc" -eq 0
        jq_check "every stream has the documented types and keys" \
            'all(.[]; (keys | sort) == (["name","size","allocated","resident","sparse","compressed"] | sort) and (.name|type)=="string" and (.size|type)=="number" and (.allocated|type)=="number" and all(.resident,.sparse,.compressed; type=="boolean"))' "$SANDBOX/out"
        jq -r '.[] | [.name,.size,.allocated] | @tsv' "$SANDBOX/out" | LC_ALL=C sort >"$SANDBOX/got"
        awk -F '\t' -v p="$item" '$1 == p { print $2 "\t" $3 "\t" $4 }' "$oracle" | LC_ALL=C sort >"$SANDBOX/want"
        check "names, lengths and allocation equal Windows for $fixture $item" cmp -s "$SANDBOX/want" "$SANDBOX/got"
        jq_check "ordinary Windows streams are neither sparse nor compressed" \
            'all(.[]; .sparse == false and .compressed == false)' "$SANDBOX/out"
        jq_check "the resident flag agrees with Windows allocation below a cluster" \
            'all(.[]; .resident == (.allocated < 4096))' "$SANDBOX/out"
        rows=$((rows + $(wc -l <"$SANDBOX/want")))
    done <"$SANDBOX/paths"
    while IFS="$(printf '\t')" read -r item name size _allocated leaf; do
        check "Windows captured the bytes for $fixture $item $name" test -f "$oracle.bytes/$leaf"
        fs.ntfs "$img" read --stream "$name" "$item" >"$SANDBOX/read" 2>"$SANDBOX/err"
        rc=$?
        if [ "$item" = /dir ]; then
            check "read keeps its directory refusal for named streams" test "$rc" -eq 1
            check "directory refusal has no stdout" test ! -s "$SANDBOX/read"
            jq_check "directory refusal names the reason" '.code == 1 and (.error|contains("is a directory"))' "$SANDBOX/err"
        else
            check "read selects Windows' $fixture $item $name" test "$rc" -eq 0
            check "selected bytes equal Get-Content -Stream output" cmp -s "$oracle.bytes/$leaf" "$SANDBOX/read"
            check "selected stream has Windows' length" test "$(wc -c <"$SANDBOX/read")" -eq "$size"
        fi
    done <"$oracle"
    check "enumeration and reading leave $fixture byte-identical" cmp -s "$img" "$SANDBOX/before.img"
done
check "Windows captured all 102 named streams (got $rows)" test "$rows" -eq 102
finish
