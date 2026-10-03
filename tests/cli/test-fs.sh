# fs.ntfs's read verbs on an image our own mkfs.ntfs makes: ls (the
# metafiles hidden unless --all asks), read, get/info with the canonical
# keys and their types, --offset, the verbs that answer `not implemented`,
# and structured errors with nothing on stdout.
source "$(dirname "$0")/lib.sh"

img="$SANDBOX/fs.img"
mkfs.ntfs -q --size 64M --label CLITEST "$img" >/dev/null 2>&1
check "mkfs.ntfs made the image" test -s "$img"

# ls /: a fresh volume has no user files. NTFS keeps its metafiles in the
# root ($MFT, $LogFile, ...: records 0 to 15); `ls` leaves them out, as
# Windows does, and `--all` shows them.
fs.ntfs "$img" ls / >"$SANDBOX/ls.json" 2>"$SANDBOX/ls.err"
status=$?
check "ls / exits 0 ($(cat "$SANDBOX/ls.err"))" test "$status" -eq 0
jq_check "ls / of a fresh volume is an empty array" '. == []' "$SANDBOX/ls.json"
fs.ntfs "$img" ls --text / >"$SANDBOX/ls.txt" 2>/dev/null
check "ls --text / of a fresh volume prints nothing" test ! -s "$SANDBOX/ls.txt"
fs.ntfs "$img" ls --all / >"$SANDBOX/all.json" 2>/dev/null
jq_check "ls --all / shows the metafiles" \
    '[.[].name] as $n | ["$MFT", "$MFTMirr", "$LogFile", "$Volume", "$AttrDef", "$Bitmap", "$Boot", "$BadClus", "$Secure", "$UpCase", "$Extend"] | all(. as $m | $n | index($m))' \
    "$SANDBOX/all.json"
jq_check "every ls --all entry has typed fields" \
    'all(.[]; (.name|type)=="string" and (.type|type)=="string" and (.size|type)=="number" and (.mode|test("^[0-7]{4}$")) and (.mtime|type)=="number" and (.record|type)=="number")' \
    "$SANDBOX/all.json"
jq_check "\$MFT is record 0 and \$Extend a directory" \
    'any(.[]; .name == "$MFT" and .record == 0 and .type == "file") and any(.[]; .name == "$Extend" and .type == "dir")' \
    "$SANDBOX/all.json"
fs.ntfs "$img" ls /missing >"$SANDBOX/lsm.out" 2>"$SANDBOX/lsm.err"
check "ls of a missing path exits 1" test $? -eq 1
check "ls of a missing path prints nothing on stdout" test ! -s "$SANDBOX/lsm.out"
jq_check "ls of a missing path says not found" '.code == 1 and (.error | test("not found"))' "$SANDBOX/lsm.err"

# read: the metafiles are files, and two of them can be checked against
# bytes that are not ours to interpret. $Boot's data is the volume's first
# 8 KiB, read through the MFT; $UpCase is the canonical table the formatter
# writes.
check "read /\$Boot is the image's first 8 KiB" cmp -s <(fs.ntfs "$img" read '/$Boot') <(head -c 8192 "$img")
check "read /\$UpCase is the canonical table" cmp -s <(fs.ntfs "$img" read '/$UpCase') "$REPO/src/upcase-canonical.bin"
fs.ntfs "$img" read '/$Boot' -o "$SANDBOX/boot.bin" >"$SANDBOX/o.out" 2>/dev/null
check "read -o writes the file and nothing on stdout" test ! -s "$SANDBOX/o.out"
check "read -o's file is the same bytes" cmp -s "$SANDBOX/boot.bin" <(head -c 8192 "$img")
check "read -o leaves no .partial behind" test ! -e "$SANDBOX/boot.bin.partial"

# get / info: every canonical key, typed; get and info identical.
fs.ntfs "$img" get >"$SANDBOX/get.json" 2>/dev/null
check "get exits 0" test $? -eq 0
jq_check "get carries every canonical key with its type" \
    '(.fs=="ntfs") and (.label|type)=="string" and (.total_bytes|type)=="number" and (.free_bytes|type)=="number" and (.block_size|type)=="number" and (.dirty|type)=="boolean" and (.ntfs|type)=="object"' \
    "$SANDBOX/get.json"
jq_check "the nested keys are the C ABI's volume info" \
    '.ntfs | [.serial_number, .ntfs_version_major, .ntfs_version_minor, .mft_record_size, .total_clusters, .free_clusters, .mft_total_records, .mft_free_records] | all(. != null)' \
    "$SANDBOX/get.json"
jq_check "the volume is clean, 4 KiB clusters, one sector short of 64 MiB" \
    '.dirty == false and .block_size == 4096 and .total_bytes == 67108352 and .free_bytes < .total_bytes' "$SANDBOX/get.json"
fs.ntfs "$img" info >"$SANDBOX/info.json" 2>/dev/null
check "info and get print the same" cmp -s "$SANDBOX/get.json" "$SANDBOX/info.json"
fs.ntfs "$img" get label >"$SANDBOX/label.json" 2>/dev/null
jq_check "get label is {\"label\": \"CLITEST\"}" '. == {"label": "CLITEST"}' "$SANDBOX/label.json"
check "get label --text is CLITEST" test "$(fs.ntfs "$img" get label --text)" = CLITEST
check "get ntfs.serial_number --text is 16 hex digits" \
    grep -qE '^[0-9a-f]{16}$' <<<"$(fs.ntfs "$img" get ntfs.serial_number --text)"
fs.ntfs "$img" get nope >"$SANDBOX/nokey.out" 2>"$SANDBOX/nokey.err"
check "get of an unknown key exits 2" test $? -eq 2
jq_check "get of an unknown key lists the keys" '.code == 2 and (.error | test("the keys are"))' "$SANDBOX/nokey.err"

# --offset: the same volume 1 MiB into a whole-disk image.
{ head -c 1048576 /dev/zero; cat "$img"; } >"$SANDBOX/whole.img"
check "--offset reaches the volume inside a larger image" \
    test "$(fs.ntfs --offset 1048576 "$SANDBOX/whole.img" get label --text)" = CLITEST
fs.ntfs "$SANDBOX/whole.img" get >"$SANDBOX/nooff.out" 2>"$SANDBOX/nooff.err"
check "without --offset the whole-disk image is not a volume" test $? -eq 1
fs.ntfs --offset 999999999 "$img" get >/dev/null 2>"$SANDBOX/past.err"
jq_check "an --offset past the end is a structured error" '.code == 1 and (.error | test("past the end"))' "$SANDBOX/past.err"

# Resize is blocked in the library: status 3, `not implemented`, nothing on stdout.
fs.ntfs "$img" resize 128M >"$SANDBOX/ni.out" 2>"$SANDBOX/ni.err"
check "resize exits 3" test $? -eq 3
check "resize prints nothing on stdout" test ! -s "$SANDBOX/ni.out"
jq_check "resize says not implemented" '.code == 3 and (.error | startswith("not implemented"))' "$SANDBOX/ni.err"

# Failures: status 1, a structured error, nothing on stdout.
fs.ntfs "$img" read / >"$SANDBOX/dir.out" 2>"$SANDBOX/dir.err"
check "read of a directory exits 1" test $? -eq 1
check "read of a directory prints nothing on stdout" test ! -s "$SANDBOX/dir.out"
jq_check "read of a directory says so" '.code == 1 and (.error | test("is a directory"))' "$SANDBOX/dir.err"
fs.ntfs "$SANDBOX/absent.img" ls / >"$SANDBOX/absent.out" 2>"$SANDBOX/absent.err"
check "a missing image exits 1" test $? -eq 1
check "a missing image prints nothing on stdout" test ! -s "$SANDBOX/absent.out"
jq_check "a missing image is a structured error" '.code == 1' "$SANDBOX/absent.err"
head -c 4096 "$img" >"$SANDBOX/cut.img"
fs.ntfs "$SANDBOX/cut.img" info >"$SANDBOX/cut.out" 2>"$SANDBOX/cut.err"
check "a truncated image exits 1" test $? -eq 1
check "a truncated image prints nothing on stdout" test ! -s "$SANDBOX/cut.out"
jq_check "a truncated image is a structured error" '.code == 1' "$SANDBOX/cut.err"

finish
