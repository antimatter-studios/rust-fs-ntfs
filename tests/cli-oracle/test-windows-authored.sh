# The other direction: volumes WINDOWS formatted and wrote, read by the
# installed fs.ntfs. The oracle is Windows itself -- the files are written
# by .NET on a real NTFS mount and hashed by Get-FileHash through ntfs.sys,
# the symlink targets are the ones Windows reports, and the compressed file
# is one Windows' compact.exe compressed -- so what is graded is our reading
# of Windows' bytes, not our reading of our own.
#
# The fixtures come from test-disks/build-windows-native-read-fixtures.ps1,
# which only Windows can run; CI's windows-native-read-fixtures job builds
# them and hands them to the `cli` job. A missing fixture FAILS, naming that.
source "$(dirname "$0")/../cli/lib.sh"

fixtures="$REPO/test-disks"
need() {
    local f
    for f in "$@"; do
        if [ ! -f "$fixtures/$f" ]; then
            fail "test-disks/$f is missing: build it on Windows with test-disks/build-windows-native-read-fixtures.ps1 (CI: the windows-native-read-fixtures job's artifact)"
            finish
        fi
    done
}
need ntfs-cli-read.img ntfs-cli-read.sha256.tsv ntfs-compressed.img ntfs-symlink.img ntfs-symlink.targets.tsv

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}

img="$fixtures/ntfs-cli-read.img"
check "the Windows-formatted volume's label is the one Format-Volume set" \
    test "$(fs.ntfs "$img" get label --text)" = CLIREAD
fs.ntfs "$img" get >"$SANDBOX/get.json" 2>/dev/null
jq_check "Windows' volume reports clean, 4 KiB clusters" '.fs == "ntfs" and .dirty == false and .block_size == 4096' "$SANDBOX/get.json"

# Every file Windows wrote: listed at the size Windows wrote, and read back
# with the SHA-256 Windows computed.
rows=0
while IFS="$(printf '\t')" read -r path size hash; do
    [ -n "$path" ] || continue
    rows=$((rows + 1))
    got="$(fs.ntfs "$img" read "$path" 2>"$SANDBOX/read.err" | sha256)"
    check "read $path has Windows' SHA-256 ($(cat "$SANDBOX/read.err"))" test "$got" = "$hash"
    fs.ntfs "$img" ls "$path" >"$SANDBOX/ls.json" 2>/dev/null
    jq_check "ls $path is one file of $size bytes" "length == 1 and .[0].type == \"file\" and .[0].size == $size" "$SANDBOX/ls.json"
done <"$fixtures/ntfs-cli-read.sha256.tsv"
check "the hash list names every file Windows wrote (8, got $rows)" test "$rows" -eq 8

fs.ntfs "$img" ls / >"$SANDBOX/root.json" 2>/dev/null
jq_check "ls / lists what Windows put in the root, sub a directory" \
    '([.[].name] | index("random.bin") and index("empty.bin") and index("sub")) and any(.[]; .name == "sub" and .type == "dir")' \
    "$SANDBOX/root.json"
fs.ntfs "$img" ls /sub/deep >"$SANDBOX/deep.json" 2>/dev/null
jq_check "ls /sub/deep is file.bin alone" '[.[].name] == ["file.bin"]' "$SANDBOX/deep.json"

# The file compact.exe compressed: the ABC pattern the builder wrote, 200000 bytes.
check "read /comp.txt of Windows' compressed volume is the pattern it wrote" \
    cmp -s <(fs.ntfs "$fixtures/ntfs-compressed.img" read /comp.txt) <(yes ABC | tr -d '\n' | head -c 200000)

# Symlinks Windows made: ls shows each as a symlink whose target is the one
# Windows reported.
fs.ntfs "$fixtures/ntfs-symlink.img" ls / >"$SANDBOX/links.json" 2>/dev/null
links=0
while IFS="$(printf '\t')" read -r name target; do
    [ -n "$name" ] || continue
    links=$((links + 1))
    jq_check "ls shows $name as a symlink to Windows' target" \
        "any(.[]; .name == \"$name\" and .type == \"symlink\" and .target == $(jq -Rn --arg t "$target" '$t'))" \
        "$SANDBOX/links.json"
done <"$fixtures/ntfs-symlink.targets.tsv"
check "Windows reported three link targets (got $links)" test "$links" -eq 3
fs.ntfs "$fixtures/ntfs-symlink.img" read /rel-file >/dev/null 2>"$SANDBOX/link.err"
jq_check "read of a symlink is refused, naming its target" '.code == 1 and (.error | test("is a link to"))' "$SANDBOX/link.err"

finish
