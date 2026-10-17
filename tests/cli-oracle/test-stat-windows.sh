# fs.ntfs stat against Windows: a volume Windows formatted and filled -- a
# file, a directory, a hard-linked file, a symlink and a junction, a file
# with named streams, one with EAs, one with attributes set and one with
# times before 1970 -- and for every path what Windows itself reports
# through a handle that opens a link rather than what it names. The file ID
# (record and sequence), the link count and each link's name and parent,
# the attributes and reparse tag, the type, the size and the four
# $STANDARD_INFORMATION times must be Windows' own, with each link reported
# rather than followed.
#
# The fixture and ntfs-stat.windows.tsv, what Windows reported, come from
# test-disks/build-windows-native-read-fixtures.ps1, which only Windows can
# run; CI's windows-native-read-fixtures job builds them and hands them to
# the `cli` job. A missing one FAILS, naming that.
source "$(dirname "$0")/../cli/lib.sh"

img="$REPO/test-disks/ntfs-stat.img"
report="$REPO/test-disks/ntfs-stat.windows.tsv"
for f in "$img" "$report"; do
    if [ ! -f "$f" ]; then
        fail "${f#"$REPO"/} is missing: build it on Windows with test-disks/build-windows-native-read-fixtures.ps1 (CI: the windows-native-read-fixtures job's artifact)"
        finish
    fi
done

# unix_time FILETIME: the seconds and nanoseconds stat prints for a time
# in 100 ns units since 1601, the seconds rounded down so the nanoseconds
# are never negative.
unix_time() {
    local d=$(($1 - 116444736000000000))
    local s=$((d / 10000000)) r=$((d % 10000000))
    if [ "$r" -lt 0 ]; then
        s=$((s - 1))
        r=$((r + 10000000))
    fi
    echo "$s $((r * 100))"
}

# attr_bit NAME: the FILE_ATTRIBUTE_* value MS-FSCC 2.6 documents for each
# name stat uses; a bit it does not name is printed as hex.
attr_bit() {
    case "$1" in
    readonly) echo 1 ;;
    hidden) echo 2 ;;
    system) echo 4 ;;
    directory) echo 16 ;;
    archive) echo 32 ;;
    normal) echo 128 ;;
    temporary) echo 256 ;;
    sparse_file) echo 512 ;;
    reparse_point) echo 1024 ;;
    compressed) echo 2048 ;;
    offline) echo 4096 ;;
    not_content_indexed) echo 8192 ;;
    encrypted) echo 16384 ;;
    integrity_stream) echo 32768 ;;
    no_scrub_data) echo 131072 ;;
    recall_on_open) echo 262144 ;;
    pinned) echo 524288 ;;
    unpinned) echo 1048576 ;;
    recall_on_data_access) echo 4194304 ;;
    0x*) echo $(($1)) ;;
    *) echo "unknown attribute name $1" >&2 && echo 0 ;;
    esac
}

rows=0
while IFS="$(printf '\t')" read -r path id links attrs tag size alloc created accessed written changed names streams eas; do
    [ -n "$path" ] || continue
    rows=$((rows + 1))
    out="$SANDBOX/stat.json"
    fs.ntfs "$img" stat "$path" >"$out" 2>"$SANDBOX/stat.err"
    rc=$?
    check "stat $path exits 0 ($(cat "$SANDBOX/stat.err"))" test "$rc" -eq 0

    # Windows' file ID is the file reference: the record in the low 48
    # bits, its sequence number in the high 16.
    record=$((16#$id & 0xFFFFFFFFFFFF))
    sequence=$((16#$id >> 48))
    jq_check "stat $path is record $record, sequence $sequence, as Windows' file ID $id" \
        ".inode == $record and .ntfs.record_number == $record and .ntfs.sequence == $sequence" "$out"
    jq_check "stat $path has the $links link(s) Windows counts" ".nlink == $links" "$out"
    jq_check "stat $path is the $size bytes Windows reports" ".size == $size" "$out"
    jq_check "stat $path has the $eas EA(s) Windows accepted" ".ntfs.ea_count == $eas" "$out"

    case "$tag" in
    a000000c) want=symlink ;;
    a0000003) want=junction ;;
    *) if [ $((attrs & 16)) -ne 0 ]; then want="dir"; else want="file"; fi ;;
    esac
    jq_check "stat $path is a $want, as Windows' attributes and reparse tag say" ".type == \"$want\"" "$out"
    if [ "$tag" = 00000000 ]; then
        jq_check "stat $path has no reparse tag, as Windows says" '.ntfs.reparse_tag == null and (has("target") | not)' "$out"
    else
        jq_check "stat $path has Windows' reparse tag 0x$tag and a target" \
            ".ntfs.reparse_tag == \"0x$tag\" and (.target | type) == \"string\"" "$out"
    fi

    # The API adds nothing NTFS does not store but NORMAL, its name for no
    # attribute at all; DUP_FILE_NAME_INDEX_PRESENT and
    # DUP_VIEW_INDEX_PRESENT (0x30000000) are NTFS's own and never reported.
    ours=0
    for name in $(jq -r '.ntfs.attributes[]' "$out"); do
        ours=$((ours | $(attr_bit "$name")))
    done
    ours=$((ours & ~0x30000000))
    [ "$ours" -eq 0 ] && ours=128
    check "stat $path has Windows' attributes ($attrs, ours $ours: $(jq -c .ntfs.attributes "$out"))" \
        test "$ours" -eq "$attrs"

    if [ "$names" != - ]; then
        count=0
        for link in $(printf '%s' "$names" | tr '|' ' '); do
            count=$((count + 1))
            parent_path="${link%/*}"
            parent="$(fs.ntfs "$img" stat "${parent_path:-/}" 2>/dev/null | jq .inode)"
            jq_check "stat $path has the link $link Windows lists, in record ${parent:-?}" \
                "any(.ntfs.file_names[]; .name == \"${link##*/}\" and .parent == ${parent:-null} and .namespace != \"dos\")" "$out"
        done
        jq_check "stat $path has the $count name(s) fsutil hardlink list gives" \
            "[.ntfs.file_names[] | select(.namespace != \"dos\")] | length == $count" "$out"
    fi
    if [ "$streams" != - ]; then
        jq_check "stat $path has the $streams named stream(s) Get-Item -Stream finds" \
            ".ntfs.named_streams == $streams" "$out"
    fi
    # Resident data takes no cluster; the one file too big for its record
    # is graded against the allocation Windows reports.
    if [ "$path" = /plain.txt ]; then
        jq_check "stat $path allocates the $alloc bytes Windows reports" ".allocated_bytes == $alloc" "$out"
    fi

    # The root's times move while Windows has the volume mounted, so they
    # were not set and are not compared.
    [ "$path" = / ] && continue
    for pair in "btime $created" "atime $accessed" "mtime $written" "ctime $changed"; do
        read -r key filetime <<<"$pair"
        read -r secs nsec <<<"$(unix_time "$filetime")"
        jq_check "stat $path: $key is Windows' $filetime, $secs s $nsec ns" \
            ".$key == $secs and .${key}_nsec == $nsec" "$out"
    done
done <"$report"
check "Windows reported thirteen paths (got $rows)" test "$rows" -eq 13

finish
