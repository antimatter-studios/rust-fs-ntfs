# fs.ntfs stat, as installed: the shared keys in the shared order and
# typed, NTFS's own under `ntfs`, the values `write` and `mkdir` produced,
# --text, the refusals, and an image left exactly as it was. What the
# values must be on a volume Windows wrote is
# tests/cli-oracle/test-stat-windows.sh.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/stat.img"
mkfs.ntfs -q --size 64M --label STAT "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"
fs.ntfs "$img" mkdir /d >/dev/null 2>&1
check "mkdir /d exits 0" test $? -eq 0
head -c 5000 /dev/urandom >"$SANDBOX/src"
fs.ntfs "$img" write /f <"$SANDBOX/src" >/dev/null 2>&1
check "write /f exits 0" test $? -eq 0
cp "$img" "$SANDBOX/before.img"

shared='["path","inode","type","mode","uid","gid","nlink","size","allocated_bytes","atime","atime_nsec","mtime","mtime_nsec","ctime","ctime_nsec","btime","btime_nsec"]'
# NTFS keeps every time to 100 ns, so no time is null; it keeps no POSIX
# owner, so uid and gid always are.
typed='(.path|type)=="string" and (.inode|type)=="number" and (.type|type)=="string"
    and (.mode|test("^[0-7]{4}$")) and .uid == null and .gid == null
    and (.nlink|type)=="number" and (.size|type)=="number" and (.allocated_bytes|type)=="number"
    and all(.atime, .atime_nsec, .mtime, .mtime_nsec, .ctime, .ctime_nsec, .btime, .btime_nsec; type=="number")
    and (.ntfs|type)=="object" and (.ntfs.record_number|type)=="number" and (.ntfs.sequence|type)=="number"
    and (.ntfs.attributes|type)=="array" and (.ntfs.file_names|type)=="array"
    and (.ntfs.named_streams|type)=="number" and (.ntfs.ea_count|type)=="number"'

for path in / /d /f; do
    fs.ntfs "$img" stat "$path" >"$SANDBOX/stat.json" 2>"$SANDBOX/stat.err"
    rc=$?
    check "stat $path exits 0 ($(cat "$SANDBOX/stat.err"))" test "$rc" -eq 0
    jq_check "stat $path starts with the shared keys, in order" \
        "[keys_unsorted[]] | .[0:17] == $shared" "$SANDBOX/stat.json"
    jq_check "stat $path has every shared key typed" "$typed" "$SANDBOX/stat.json"
    jq_check "stat $path reports the path it was asked" ".path == \"$path\"" "$SANDBOX/stat.json"
    jq_check "stat $path: the inode is the record number" '.inode == .ntfs.record_number' "$SANDBOX/stat.json"
done

fs.ntfs "$img" stat / >"$SANDBOX/root.json" 2>/dev/null
jq_check "stat / is the root directory, record 5, named . in itself" \
    '.type == "dir" and .inode == 5 and .nlink == 1 and any(.ntfs.file_names[]; .name == "." and .parent == 5)' \
    "$SANDBOX/root.json"
fs.ntfs "$img" stat /d >"$SANDBOX/d.json" 2>/dev/null
jq_check "stat /d is a directory with one name, in the root" \
    '.type == "dir" and .mode == "0755" and .nlink == 1 and .size == 0 and ([.ntfs.file_names[] | select(.namespace != "dos") | [.name, .parent]] == [["d", 5]])' \
    "$SANDBOX/d.json"
fs.ntfs "$img" stat /f >"$SANDBOX/f.json" 2>/dev/null
jq_check "stat /f is a 5000-byte file with one name, in the root" \
    '.type == "file" and .mode == "0644" and .size == 5000 and .nlink == 1 and .allocated_bytes >= 5000 and ([.ntfs.file_names[] | select(.namespace != "dos") | [.name, .parent]] == [["f", 5]])' \
    "$SANDBOX/f.json"
jq_check "stat /f: no reparse point, object ID, named stream or EA, and no target" \
    '.ntfs.reparse_tag == null and .ntfs.object_id == null and .ntfs.named_streams == 0 and .ntfs.ea_count == 0 and (has("target") | not)' \
    "$SANDBOX/f.json"
jq_check "stat /f: every \$FILE_NAME carries its four times" \
    'all(.ntfs.file_names[]; all(.btime, .btime_nsec, .mtime, .mtime_nsec, .ctime, .ctime_nsec, .atime, .atime_nsec; type == "number"))' \
    "$SANDBOX/f.json"
fs.ntfs "$img" ls / >"$SANDBOX/ls.json" 2>/dev/null
check "stat /f and ls / agree on the record, the mode and the mtime" \
    test "$(jq -c '[.inode, .mode, .mtime]' "$SANDBOX/f.json")" = "$(jq -c '.[] | select(.name == "f") | [.record, .mode, .mtime]' "$SANDBOX/ls.json")"

fs.ntfs "$img" stat --text /f >"$SANDBOX/f.txt" 2>/dev/null
check "stat --text /f exits 0" test $? -eq 0
check "stat --text /f prints key: value lines" grep -qx 'size: 5000' "$SANDBOX/f.txt"
check "stat --text /f names the type" grep -qx 'type: file' "$SANDBOX/f.txt"
check "stat --text /f prints NTFS's own keys dotted" grep -qx 'ntfs.record_number: [0-9]*' "$SANDBOX/f.txt"

# Refusals: a structured error, nothing on stdout.
fs.ntfs "$img" stat /missing >"$SANDBOX/m.out" 2>"$SANDBOX/m.err"
check "stat of a missing path exits 1" test $? -eq 1
check "stat of a missing path prints nothing on stdout" test ! -s "$SANDBOX/m.out"
jq_check "stat of a missing path says not found" '.code == 1 and (.error | test("not found"))' "$SANDBOX/m.err"
fs.ntfs "$img" stat >"$SANDBOX/u.out" 2>"$SANDBOX/u.err"
check "stat with no path is a usage error" test $? -eq 2
check "stat with no path prints nothing on stdout" test ! -s "$SANDBOX/u.out"

check "stat left the image as it was" cmp -s "$img" "$SANDBOX/before.img"

finish
