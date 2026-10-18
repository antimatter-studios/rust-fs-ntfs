#!/usr/bin/env bash
# cli-oracle.sh STEP IMAGE [ARG...] -- one host-side step of a Windows
# oracle scenario for the command-line tools (the `cli-*` ops in
# fs-windows-test-harness.toml; the scenarios are the `cli` group in
# test-matrix.json).
#
# The tools make and change the image here; Windows then grades it
# (win-chkdsk, win-dirty-query, win-cli-verify). Each step runs the
# multi-call binary under its repository name -- `rust-fs-ntfs mkfs ...` is
# the same program as `mkfs.ntfs ...` and needs no links, which a Windows
# checkout does not have -- and checks what the tool itself reported, so a
# scenario fails at the step that went wrong rather than at chkdsk.
#
# STEPS
#   mkfs IMAGE SIZE_MIB LABEL        mkfs.ntfs --size, exit 0, the label read back
#   mkfs-plan IMAGE SIZE_MIB LABEL CLUSTER RECORD SERIAL
#                                    mkfs.ntfs --dry-run reports a plan and creates
#                                    nothing; the same command without --dry-run
#                                    applies that plan. IMAGE.plan then holds the
#                                    plan's fields, one `key<TAB>value` per line,
#                                    for win-ntfsinfo.ps1
#   set-dirty IMAGE                  fs.ntfs set dirty true
#   corrupt IMAGE mirror|signature   damage the volume by its on-disk layout
#   fsck-finds IMAGE KIND            fsck.ntfs exits 4 and reports a finding of KIND
#   populate IMAGE LABEL             fs.ntfs mkdir, write (every size that changes
#                                    how NTFS stores a file, replacements shorter
#                                    and longer) and set label; IMAGE.manifest then
#                                    lists every file's SHA-256, every directory and
#                                    the label, for win-cli-verify.ps1
#   replay IMAGE SNAPSHOT            IMAGE is test-disks/windows-interrupted-SNAPSHOT,
#                                    whose log holds work; fsck.ntfs reports it,
#                                    fsck.ntfs -y replays it, and IMAGE.manifest is
#                                    what Windows listed after recovering the same
#                                    image itself, for win-cli-verify.ps1
#   replay-stopped IMAGE SNAPSHOT    IMAGE is test-disks/windows-interrupted-SNAPSHOT,
#                                    whose replay is cut right after its first undo
#                                    write, as a power cut leaves it, so the log
#                                    still holds work for Windows' restart to find;
#                                    IMAGE.manifest is what Windows listed after
#                                    recovering the same image itself
#   replay-lsn IMAGE SNAPSHOT        IMAGE is test-disks/windows-interrupted-SNAPSHOT
#                                    replayed by fsck.ntfs -y; IMAGE.lsns holds the
#                                    LSNs the replay left on the volume (#649)
#   lsn-after-windows IMAGE          IMAGE, back from Windows, carries no restart
#                                    area naming an LSN at or below IMAGE.lsns'
#                                    highest block LSN (#649)
#   write-after-replay IMAGE SNAPSHOT
#                                    IMAGE is test-disks/windows-interrupted-SNAPSHOT;
#                                    fsck.ntfs reports its log, fs.ntfs mkdir is
#                                    refused with every byte left as it was,
#                                    fsck.ntfs -y replays the log, then fs.ntfs mkdir
#                                    and write, and fsck.ntfs after finds nothing.
#                                    IMAGE.manifest is Windows' listing of its own
#                                    recovery plus what fs.ntfs wrote
#   empty-directory IMAGE            fs.ntfs fills /kept and /gone past one index
#                                    level, rm removes half of /kept and all of
#                                    /gone in shuffled order, and rmdir removes
#                                    /gone; IMAGE.manifest lists /kept's survivors
#                                    for win-cli-verify.ps1
#   rename IMAGE                     fs.ntfs mv renames within a directory: the
#                                    same length, longer, shorter, case only, a
#                                    directory, data that is not resident, a
#                                    Unicode name, and a third of a directory
#                                    filled past one index level. A move to
#                                    another directory and a name already there
#                                    are refused with every byte left as it was.
#                                    IMAGE.manifest lists every file's SHA-256
#                                    under its new name, the exact case of each
#                                    new name and each old name now absent
#   link IMAGE                       fs.ntfs ln adds hard links -- in the same
#                                    directory, in others, to data that is not
#                                    resident, and to one file past the names its
#                                    base record holds -- and removes an original
#                                    name; ln -s makes symbolic links, relative and
#                                    from the root. A hard link to a directory and
#                                    a name already there are refused with every
#                                    byte left as it was.
#                                    IMAGE.manifest lists every name's SHA-256,
#                                    each file's full set of names and each
#                                    symbolic link's target and the bytes it reaches
#   attrlist-links IMAGE             fs.ntfs ln gives one file more names than its
#                                    record holds, in two directories, so they go
#                                    through an $ATTRIBUTE_LIST; rm takes the first
#                                    name and every third, mv renames every third
#                                    longer, and ln adds names in the room left,
#                                    fs.ntfs stat counting every name each time.
#                                    IMAGE.manifest lists every remaining name's
#                                    SHA-256, the file's full set of names, each
#                                    new name's exact case and each name now absent
#   attrlist-data IMAGE              rust-ntfs stream and ea fill two files' base
#                                    records so each gains an $ATTRIBUTE_LIST, one
#                                    with its unnamed stream already in the base
#                                    record and one whose stream grows after; fs.ntfs
#                                    write, write --at and truncate then rewrite,
#                                    patch and resize each unnamed stream, every
#                                    named stream read back unchanged each time.
#                                    IMAGE.manifest lists each file's final size
#                                    and SHA-256
#   attrlist-split-extents IMAGE     rust-ntfs stream and ea give /split.dat an
#                                    $ATTRIBUTE_LIST; fs.ntfs cuts the free space
#                                    into one-cluster holes and writes, patches,
#                                    rewrites and truncates /split.dat's unnamed
#                                    stream over them, so its run list is split
#                                    into extents the list names. IMAGE.manifest
#                                    lists the file's final size and SHA-256
#   mkdir-parents IMAGE              fs.ntfs mkdir -p makes missing chains: three
#                                    levels, under existing directories through
#                                    names in another case, through `.` and `..`,
#                                    twelve levels, Unicode names, and one into
#                                    each of 300 directories filled past one index
#                                    level. A chain already there writes nothing; a
#                                    file in the way and names Windows cannot address
#                                    are refused with every byte left as it was.
#                                    IMAGE.manifest lists every directory made, its
#                                    exact case, the name `..` passed over as absent
#                                    and the chains Windows must refuse as well
#   truncate IMAGE                   resize resident and non-resident files;
#                                    Windows repeats each SetLength sequence
#                                    independently and compares content and length
#   grow-mft IMAGE SNAPSHOT        IMAGE is what Windows recovered from
#                                    test-disks/windows-interrupted-SNAPSHOT, whose
#                                    $MFT:$Bitmap outruns $MFT; fs.ntfs writes files
#                                    until $MFT has grown twice, and fsck.ntfs after
#                                    finds nothing. IMAGE.manifest is Windows' listing
#                                    plus every file fs.ntfs wrote
#
# The binary is target/release/rust-fs-ntfs (built with `--features cli`),
# or RUST_FS_NTFS. JSON is read with grep, not jq: the Windows runner's Git
# Bash is not promised to carry jq, and each check is one field.
set -euo pipefail

# Git Bash rewrites any argument that looks like a POSIX path before it
# reaches a native program, so `fs.ntfs IMAGE mkdir /d` arrived as `D:/`.
# The paths passed to the tool are paths inside the NTFS image, never on
# the host: nothing here wants the rewrite.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${RUST_FS_NTFS:-$REPO/target/release/rust-fs-ntfs}"

die() {
    echo "cli-oracle: $*" >&2
    exit 1
}

[ $# -ge 2 ] || die "usage: cli-oracle.sh STEP IMAGE [ARG...]"
step="$1"
image="$2"
shift 2

[ -x "$BIN" ] || [ -x "$BIN.exe" ] ||
    die "no $BIN: build it with \`cargo build --release --locked --features cli --bin rust-fs-ntfs\`"

# run TOOL ARG...: the tool's report in $out, its status in $status. Quiet:
# a failing step prints the report it is failing on, a passing one a line.
run() {
    set +e
    out="$("$BIN" "$@" 2>"$image.cli-err")"
    status=$?
    set -e
}

# has PATTERN: the report carries PATTERN (a `"key": value` line).
has() { grep -q -- "$1" <<<"$out"; }

# The on-disk layout, read from the boot sector (MS-FSCC), not from the
# code under test: where the damage goes must not be the tool's answer.
u8() { od -An -tu1 -j "$2" -N 1 "$1" | tr -d ' \n\r'; }
u16() { od -An -tu2 -j "$2" -N 2 "$1" | tr -d ' \n\r'; }
u64() { od -An -tu8 -j "$2" -N 8 "$1" | tr -d ' \n\r'; }
cluster_size() { echo $(($(u16 "$1" 11) * $(u8 "$1" 13))); }
record_size() {
    local n
    n="$(u8 "$1" 64)"
    if [ "$n" -lt 128 ]; then echo $((n * $(cluster_size "$1"))); else echo $((1 << (256 - n))); fi
}
poke() { printf "$3" | dd of="$1" bs=1 seek="$2" conv=notrunc 2>/dev/null; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
random() { if [ "$2" -gt 0 ]; then head -c "$2" /dev/urandom >"$1"; else : >"$1"; fi; }

# lsns IMAGE: the LSNs IMAGE's raw bytes carry, one "<kind> <lsn>" per
# line (examples/log_lsns.rs).
lsns() {
    cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" --example log_lsns -- "$1" ||
        die "log_lsns $1 failed"
}

# windows_listing SNAPSHOT: what Windows listed after recovering the same
# image itself -- path, size and Get-FileHash SHA-256 -- as win-cli-verify
# reads it, to IMAGE.manifest.
windows_listing() {
    awk -F '\t' 'NF == 3 { printf "file\t/%s\t%s\t%s\n", $1, $2, $3 }' \
        "$REPO/test-disks/windows-interrupted-$1.recovered.manifest" >"$image.manifest"
    [ -s "$image.manifest" ] || die "no manifest for snapshot $1"
}

# unpack_interrupted SNAPSHOT: IMAGE is the volume Windows left mid-write,
# and fsck.ntfs must see its log holding work.
unpack_interrupted() {
    mkdir -p "$(dirname "$image")"
    gzip -dc "$REPO/test-disks/windows-interrupted-$1.img.gz" >"$image" ||
        die "no test-disks/windows-interrupted-$1.img.gz"
    run fsck "$image"
    [ "$status" -eq 4 ] || die "fsck.ntfs on the volume Windows left mid-write exited $status, not 4: $out"
    has '"kind": "logfile"' || die "fsck.ntfs did not report the log holding work: $out"
}

case "$step" in
    mkfs)
        [ $# -eq 2 ] || die "mkfs IMAGE SIZE_MIB LABEL"
        mkdir -p "$(dirname "$image")"
        rm -f "$image"
        run mkfs --size "$1M" --label "$2" "$image"
        [ "$status" -eq 0 ] || die "mkfs.ntfs exited $status: $(cat "$image.cli-err")"
        has "\"label\": \"$2\"" || die "mkfs.ntfs's report does not read back the label $2"
        has '"formatted": true' || die "mkfs.ntfs's report does not say it formatted"
        ;;
    mkfs-plan)
        [ $# -eq 5 ] || die "mkfs-plan IMAGE SIZE_MIB LABEL CLUSTER RECORD SERIAL"
        mkdir -p "$(dirname "$image")"
        rm -f "$image" "$image.plan"
        flags=(--size "$1M" --label "$2" --cluster-size "$3" --mft-record-size "$4" --serial "$5")
        run mkfs --dry-run "${flags[@]}" "$image"
        [ "$status" -eq 0 ] || die "mkfs.ntfs --dry-run exited $status: $(cat "$image.cli-err")"
        [ ! -e "$image" ] || die "mkfs.ntfs --dry-run created $image"
        # The plan is the report's one nested object, so its fields are the
        # only ones indented twice in the pretty JSON.
        plan_fields() {
            awk -F '": ' '/^    "[a-z_]+": / {
                key = $1; sub(/^    "/, "", key)
                value = $2; sub(/,$/, "", value); gsub(/^"|"$/, "", value)
                print key "\t" value
            }' <<<"$out"
        }
        planned="$(plan_fields)"
        [ -n "$planned" ] || die "mkfs.ntfs --dry-run reported no plan: $out"
        for key in bytes_per_sector cluster_size mft_record_size total_sectors total_clusters mft_lcn mftmirr_lcn serial label; do
            grep -q "^$key"$'\t' <<<"$planned" || die "the dry run's plan has no $key: $out"
        done
        run mkfs "${flags[@]}" "$image"
        [ "$status" -eq 0 ] || die "mkfs.ntfs exited $status: $(cat "$image.cli-err")"
        [ "$(plan_fields)" = "$planned" ] ||
            die "formatting applied a plan other than the dry run's: $out"
        printf '%s\n' "$planned" >"$image.plan"
        ;;
    set-dirty)
        run fs "$image" set dirty true
        [ "$status" -eq 0 ] || die "fs.ntfs set dirty true exited $status: $(cat "$image.cli-err")"
        run fs "$image" get dirty --text
        [ "$out" = true ] || die "get dirty after set dirty true says '$out'"
        ;;
    corrupt)
        [ $# -eq 1 ] || die "corrupt IMAGE mirror|signature"
        cluster="$(cluster_size "$image")"
        record="$(record_size "$image")"
        case "$1" in
            # One byte of $MFTMirr's copy of record 0 ($MFT), well inside
            # the record: the mirror (+0x38) no longer agrees with $MFT.
            mirror) poke "$image" $(($(u64 "$image" 56) * cluster + 200)) '\x5a' ;;
            # The FILE signature of record 13, a reserved record mkfs.ntfs
            # marks in use: $MFT (+0x30) is one run on a fresh volume.
            signature) poke "$image" $(($(u64 "$image" 48) * cluster + 13 * record)) 'XXXX' ;;
            *) die "corrupt: '$1' is not mirror or signature" ;;
        esac
        ;;
    fsck-finds)
        [ $# -eq 1 ] || die "fsck-finds IMAGE KIND"
        run fsck "$image"
        [ "$status" -eq 4 ] || die "fsck.ntfs exited $status, not 4 (errors left): $out"
        has "\"kind\": \"$1\"" || die "fsck.ntfs reported no $1 finding: $out"
        ;;
    populate)
        [ $# -eq 1 ] || die "populate IMAGE LABEL"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        for dir in /d /d/e; do
            run fs "$image" mkdir "$dir"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $dir exited $status: $(cat "$image.cli-err")"
            printf 'dir\t%s\n' "$dir" >>"$manifest"
        done
        # path:size, then path:size again for a replacement.
        for spec in /empty.bin:0 /one.bin:1 /resident.bin:500 /cluster.bin:4096 \
            /cluster-plus-one.bin:4097 /random.bin:1048576 /d/e/deep.bin:5000 \
            "/na$(printf '\303\257')ve-$(printf '\346\227\245\346\234\254').txt:42" \
            /shrunk.bin:4097 /shrunk.bin:1 /grown.bin:1 /grown.bin:1048576; do
            path="${spec%:*}"
            size="${spec##*:}"
            random "$work/data" "$size"
            set +e
            "$BIN" fs "$image" write "$path" <"$work/data" >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write $path ($size bytes) exited $status: $(cat "$image.cli-err")"
            grep -v "^file	$path	" "$manifest" >"$work/m" || true
            printf 'file\t%s\t%s\t%s\n' "$path" "$size" "$(sha "$work/data")" >>"$work/m"
            cp "$work/m" "$manifest"
        done
        run fs "$image" set label "$1"
        [ "$status" -eq 0 ] || die "fs.ntfs set label exited $status: $(cat "$image.cli-err")"
        printf 'label\t%s\n' "$1" >>"$manifest"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the writes exited $status: $out"
        ;;
    empty-directory)
        [ $# -eq 0 ] || die "empty-directory IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        for dir in /kept /gone; do
            run fs "$image" mkdir "$dir"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $dir exited $status: $(cat "$image.cli-err")"
        done
        printf 'dir\t/kept\n' >>"$manifest"
        # 300 names of varying length in each: past one index level.
        for i in $(seq 1 300); do
            printf 'name-%03d-%s\n' "$i" "$(printf '%*s' $((i % 23)) '' | tr ' ' x)"
        done >"$work/names"
        while read -r name; do
            random "$work/$name" 64
            for dir in /kept /gone; do
                set +e
                "$BIN" fs "$image" write "$dir/$name" <"$work/$name" >/dev/null 2>"$image.cli-err"
                status=$?
                set -e
                [ "$status" -eq 0 ] || die "fs.ntfs write $dir/$name exited $status: $(cat "$image.cli-err")"
            done
        done <"$work/names"
        # A seeded shuffle: interior index entries go as well as leaf ones.
        awk 'BEGIN { srand(495) } { print rand() "\t" $0 }' "$work/names" | sort | cut -f2 >"$work/order"
        n=0
        while read -r name; do
            n=$((n + 1))
            if [ $((n % 2)) -eq 0 ]; then
                run fs "$image" rm "/kept/$name"
                [ "$status" -eq 0 ] || die "fs.ntfs rm /kept/$name exited $status: $(cat "$image.cli-err")"
            else
                printf 'file\t%s\t%s\t%s\n' "/kept/$name" 64 "$(sha "$work/$name")" >>"$manifest"
            fi
            run fs "$image" rm "/gone/$name"
            [ "$status" -eq 0 ] || die "fs.ntfs rm /gone/$name exited $status: $(cat "$image.cli-err")"
        done <"$work/order"
        run fs "$image" rmdir /gone
        [ "$status" -eq 0 ] || die "fs.ntfs rmdir /gone exited $status: $(cat "$image.cli-err")"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the removals exited $status: $out"
        ;;
    rename)
        [ $# -eq 0 ] || die "rename IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$work/files"
        : >"$manifest"
        # put PATH SIZE: fs.ntfs writes SIZE random bytes to PATH, kept in
        # $work under PATH's record in $work/files.
        put() {
            local key
            key="$(printf '%s' "$1" | od -An -tx1 | tr -d ' \n')"
            random "$work/$key" "$2"
            set +e
            "$BIN" fs "$image" write "$1" <"$work/$key" >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write $1 ($2 bytes) exited $status: $(cat "$image.cli-err")"
            printf '%s\t%s\t%s\n' "$1" "$2" "$key" >>"$work/files"
        }
        # renamed SOURCE PATH: fs.ntfs mv renames SOURCE to PATH, and every file
        # recorded under SOURCE, or under it as a directory, is now under
        # PATH. Windows must list PATH's last name exactly so, and find
        # SOURCE no more unless the two differ only in case.
        renamed() {
            run fs "$image" mv "$1" "$2"
            [ "$status" -eq 0 ] || die "fs.ntfs mv $1 $2 exited $status: $(cat "$image.cli-err")"
            has "\"path\": \"$2\"" && has '"moved": true' || die "fs.ntfs mv $1 $2 reported: $out"
            awk -F '\t' -v from="$1" -v to="$2" 'BEGIN { OFS = FS }
                $1 == from { $1 = to }
                index($1, from "/") == 1 { $1 = to substr($1, length(from) + 1) }
                { print }' "$work/files" >"$work/f"
            cp "$work/f" "$work/files"
            printf 'name\t%s\n' "$2" >>"$manifest"
            if [ "$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]')" != "$(printf '%s' "$2" | tr '[:lower:]' '[:upper:]')" ]; then
                printf 'absent\t%s\n' "$1" >>"$manifest"
            fi
        }
        # refused STATUS SOURCE PATH: fs.ntfs mv exits STATUS and writes nothing.
        refused() {
            local before
            before="$(sha "$image")"
            run fs "$image" mv "$2" "$3"
            [ "$status" -eq "$1" ] || die "fs.ntfs mv $2 $3 exited $status, not $1: $(cat "$image.cli-err")"
            [ "$(sha "$image")" = "$before" ] || die "the refused fs.ntfs mv $2 $3 wrote to the volume"
        }

        for dir in /d /d/sub /e /big; do
            run fs "$image" mkdir "$dir"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $dir exited $status: $(cat "$image.cli-err")"
        done
        put /alpha.bin 500
        put /short.txt 5000
        put /long-file-name.data 4097
        put /readme.txt 42
        put /d/sub/inner.bin 1048576
        put /naive.txt 7
        put /e/stay.txt 9
        # 300 names of varying length: past one index level in 4 KiB blocks.
        for i in $(seq 1 300); do
            printf 'name-%03d-%s\n' "$i" "$(printf '%*s' $((i % 23)) '' | tr ' ' x)"
        done >"$work/names"
        while read -r name; do put "/big/$name" 64; done <"$work/names"

        renamed /alpha.bin /omega.bin                                  # the same length
        renamed /short.txt "/a much longer name than before.txt"       # longer
        renamed /long-file-name.data /x.dat                            # shorter, and fits 8.3
        renamed /readme.txt /README.txt                                # case only
        renamed /d /docs                                               # a directory
        renamed /docs/sub/inner.bin /docs/sub/i.bin                    # deeper down
        renamed /naive.txt "/na$(printf '\303\257')ve-$(printf '\346\227\245\346\234\254').txt"
        # A seeded third of /big: case only, longer (sorting last) and
        # shorter (sorting first), so entries move between index blocks.
        awk 'BEGIN { srand(645) } { print rand() "\t" $0 }' "$work/names" | sort | cut -f2 | head -n 100 >"$work/order"
        n=0
        while read -r name; do
            n=$((n + 1))
            case $((n % 3)) in
                0) new="$(printf '%s' "$name" | tr '[:lower:]' '[:upper:]')" ;;
                1) new="zz-renamed-$name" ;;
                2) new="$(printf 'a%03d' "$n")" ;;
            esac
            renamed "/big/$name" "/big/$new"
        done <"$work/order"

        # Refusals, each leaving every byte as it was: another directory
        # (a move this driver cannot make, exit 3), a name already there as
        # Windows compares names, and a new name under a file.
        refused 3 /omega.bin /docs/omega.bin
        refused 3 /docs/sub /e/sub
        refused 1 /x.dat /OMEGA.BIN
        refused 1 /e/stay.txt /e/STAY.TXT/x

        printf 'dir\t%s\n' /docs /docs/sub /e /big >>"$manifest"
        while IFS="$(printf '\t')" read -r path size key; do
            printf 'file\t%s\t%s\t%s\n' "$path" "$size" "$(sha "$work/$key")" >>"$manifest"
        done <"$work/files"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the renames exited $status: $out"
        ;;
    link)
        [ $# -eq 0 ] || die "link IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        # put PATH SIZE KEY: fs.ntfs writes SIZE random bytes to PATH, kept
        # as $work/KEY.
        put() {
            random "$work/$3" "$2"
            set +e
            "$BIN" fs "$image" write "$1" <"$work/$3" >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write $1 ($2 bytes) exited $status: $(cat "$image.cli-err")"
        }
        # linked FILE PATH: fs.ntfs ln makes PATH another name for FILE.
        linked() {
            run fs "$image" ln "$1" "$2"
            [ "$status" -eq 0 ] || die "fs.ntfs ln $1 $2 exited $status: $(cat "$image.cli-err")"
            has "\"path\": \"$2\"" && has '"linked": true' || die "fs.ntfs ln $1 $2 reported: $out"
        }
        # symlinked TEXT PATH KEY: fs.ntfs ln -s makes PATH a symbolic link
        # to TEXT, a file holding KEY's bytes. Windows must read TEXT as the
        # link's target and those bytes through PATH.
        symlinked() {
            run fs "$image" ln -s "$1" "$2"
            [ "$status" -eq 0 ] || die "fs.ntfs ln -s $1 $2 exited $status: $(cat "$image.cli-err")"
            has "\"path\": \"$2\"" && has '"linked": true' || die "fs.ntfs ln -s $1 $2 reported: $out"
            printf 'symlink\t%s\t%s\t%s\t%s\n' "$2" "$1" "$(wc -c <"$work/$3" | tr -d ' ')" "$(sha "$work/$3")" >>"$manifest"
        }
        # refused STATUS ARG...: fs.ntfs ln ARG... exits STATUS and writes nothing.
        refused() {
            local want="$1" before
            shift
            before="$(sha "$image")"
            run fs "$image" ln "$@"
            [ "$status" -eq "$want" ] || die "fs.ntfs ln $* exited $status, not $want: $(cat "$image.cli-err")"
            [ "$(sha "$image")" = "$before" ] || die "the refused fs.ntfs ln $* wrote to the volume"
        }
        # names KEY SIZE NAME...: every NAME holds KEY's SIZE bytes, and the
        # NAMEs are all the file's names.
        names() {
            local key="$1" size="$2" name
            shift 2
            for name in "$@"; do
                printf 'file\t%s\t%s\t%s\n' "$name" "$size" "$(sha "$work/$key")" >>"$manifest"
            done
            (IFS="$(printf '\t')" && printf 'links\t%s\n' "$*") >>"$manifest"
        }

        for dir in /d /d/sub /e; do
            run fs "$image" mkdir "$dir"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $dir exited $status: $(cat "$image.cli-err")"
        done
        put /a.txt 500 a
        put /big.bin 1048576 big
        put /d/f.txt 4097 f
        put /many.txt 64 many

        linked /a.txt /a-also.txt         # the same directory
        linked /a.txt /d/a-in-d.txt       # another directory
        linked /a.txt /d/sub/A-DEEP.TXT   # deeper down, in capitals
        linked /big.bin /e/big-link.bin   # data that is not resident
        linked /d/f.txt /f-up.txt         # up to the root
        # /many.txt takes names past what its base record holds, as Windows
        # lets a file have up to 1024: those the base record has no room for
        # go through an $ATTRIBUTE_LIST. Forty is several records' worth.
        n=0
        while [ "$n" -lt 40 ]; do
            linked /many.txt "/e/many-$n.txt"
            n=$((n + 1))
        done
        # The first name goes, and the file stays under its others.
        run fs "$image" rm /a.txt
        [ "$status" -eq 0 ] || die "fs.ntfs rm /a.txt exited $status: $(cat "$image.cli-err")"

        symlinked a-also.txt /rel.txt a                # relative, the same directory
        symlinked ../big.bin /d/up.bin big             # relative, up a level
        symlinked ../d/sub/A-DEEP.TXT /e/deep.txt a    # relative, across the tree
        symlinked /d/f.txt /e/abs.txt f                # from the volume's root

        # Refusals, each leaving every byte as it was: a directory, a name
        # already there as Windows compares names, and a directory that is
        # not there.
        refused 1 /d /d-link
        refused 1 /f-up.txt /A-ALSO.TXT
        refused 1 /a-also.txt /nowhere/x.txt

        printf 'dir\t%s\n' /d /d/sub /e >>"$manifest"
        printf 'absent\t%s\n' /a.txt >>"$manifest"
        names a 500 /a-also.txt /d/a-in-d.txt /d/sub/A-DEEP.TXT
        names big 1048576 /big.bin /e/big-link.bin
        names f 4097 /d/f.txt /f-up.txt
        many=(/many.txt)
        for i in $(seq 0 $((n - 1))); do many+=("/e/many-$i.txt"); done
        names many 64 "${many[@]}"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the links exited $status: $out"
        ;;
    attrlist-links)
        [ $# -eq 0 ] || die "attrlist-links IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        # One file takes more names than its record can hold, so its
        # $FILE_NAMEs go through an $ATTRIBUTE_LIST into extension records,
        # as Windows lays them out (#662). Each name is twelve characters:
        # its $FILE_NAME is a 0x18-byte header, 0x42 bytes of fields and 24
        # of UTF-16, 120 bytes rounded up to 8, so COUNT of them outgrow the
        # record before any other attribute is counted.
        count=48
        [ $((count * 120)) -gt "$(record_size "$image")" ] ||
            die "$count names fit in one $(record_size "$image")-byte record: the file would need no list"
        # linked, unlinked, renamed: fs.ntfs ln, rm and mv, each checked by
        # what it reported. $work/names holds every name the file has.
        linked() {
            run fs "$image" ln "$1" "$2"
            [ "$status" -eq 0 ] || die "fs.ntfs ln $1 $2 exited $status: $(cat "$image.cli-err")"
            has "\"path\": \"$2\"" && has '"linked": true' || die "fs.ntfs ln $1 $2 reported: $out"
            echo "$2" >>"$work/names"
        }
        unlinked() {
            run fs "$image" rm "$1"
            [ "$status" -eq 0 ] || die "fs.ntfs rm $1 exited $status: $(cat "$image.cli-err")"
            grep -vxF -- "$1" "$work/names" >"$work/kept" || true
            cat "$work/kept" >"$work/names"
            printf 'absent\t%s\n' "$1" >>"$manifest"
        }
        renamed() {
            run fs "$image" mv "$1" "$2"
            [ "$status" -eq 0 ] || die "fs.ntfs mv $1 $2 exited $status: $(cat "$image.cli-err")"
            has "\"path\": \"$2\"" && has '"moved": true' || die "fs.ntfs mv $1 $2 reported: $out"
            grep -vxF -- "$1" "$work/names" >"$work/kept" || true
            { cat "$work/kept"; echo "$2"; } >"$work/names"
            printf 'absent\t%s\nname\t%s\n' "$1" "$2" >>"$manifest"
        }
        # nlinks: fs.ntfs stat counts exactly the names in $work/names.
        nlinks() {
            local want
            want="$(wc -l <"$work/names" | tr -d ' ')"
            run fs "$image" stat "$(head -n 1 "$work/names")"
            [ "$status" -eq 0 ] || die "fs.ntfs stat exited $status: $(cat "$image.cli-err")"
            grep -qE "\"nlink\": ${want}([^0-9]|\$)" <<<"$out" || die "fs.ntfs stat does not count $want names: $out"
        }
        # name_of I: the Ith name, alternately in /m and /n.
        name_of() {
            if [ $(($1 % 2)) -eq 0 ]; then printf '/m/name-%03d.txt' "$1"; else printf '/n/name-%03d.txt' "$1"; fi
        }

        for dir in /m /n; do
            run fs "$image" mkdir "$dir"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $dir exited $status: $(cat "$image.cli-err")"
        done
        random "$work/data" 700
        set +e
        "$BIN" fs "$image" write /m/first.dat <"$work/data" >/dev/null 2>"$image.cli-err"
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "fs.ntfs write /m/first.dat exited $status: $(cat "$image.cli-err")"
        echo /m/first.dat >"$work/names"
        for i in $(seq 0 $((count - 1))); do
            linked /m/first.dat "$(name_of "$i")"
        done
        nlinks
        # The first name goes, which the base record held; then every third
        # name, and every third after that is renamed longer, so it may no
        # longer fit in the record that held it; then new names take the
        # room the others left.
        unlinked /m/first.dat
        for i in $(seq 0 $((count - 1))); do
            case $((i % 3)) in
                0) unlinked "$(name_of "$i")" ;;
                1) renamed "$(name_of "$i")" "$(dirname "$(name_of "$i")")/renamed-to-a-longer-name-$i.txt" ;;
            esac
        done
        for i in $(seq 0 7); do
            linked "$(head -n 1 "$work/names")" "/n/again-$i.txt"
        done
        nlinks

        printf 'dir\t%s\n' /m /n >>"$manifest"
        hash="$(sha "$work/data")"
        while read -r name; do
            printf 'file\t%s\t700\t%s\n' "$name" "$hash" >>"$manifest"
        done <"$work/names"
        printf 'links\t%s\n' "$(paste -s -d '\t' - <"$work/names")" >>"$manifest"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the links, unlinks and renames exited $status: $out"
        ;;
    attrlist-data)
        [ $# -eq 0 ] || die "attrlist-data IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        # The named streams and the EA are written by the harness binary,
        # rust-ntfs stream and ea: fs.ntfs reads them but has no writer.
        HARNESS="${RUST_NTFS:-$REPO/target/release/rust-ntfs}"
        [ -x "$HARNESS" ] || [ -x "$HARNESS.exe" ] ||
            die "no $HARNESS: build it with \`cargo build --release --locked --features harness --bin rust-ntfs\`"
        # Twelve 120-byte resident streams, each a 0x18-byte header, a name
        # of two or three UTF-16 letters aligned to 8 and the value, outgrow the
        # record before the EA or the unnamed stream is counted, so each
        # file's base record fills and it gains an $ATTRIBUTE_LIST (#663).
        streams=12
        [ $((streams * (0x18 + 8 + 120))) -gt "$(record_size "$image")" ] ||
            die "$streams streams fit in one $(record_size "$image")-byte record: the file would need no list"
        listed() {
            local i
            set +e
            MSYS_NO_PATHCONV=1 "$HARNESS" ea "$image" "$1" USER.TAG --content "$(printf 'e%.0s' $(seq 1 64))" \
                >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "rust-ntfs ea $1 USER.TAG exited $status: $(cat "$image.cli-err")"
            for i in $(seq 1 "$streams"); do
                { printf 's%02d of %s ' "$i" "$1"; head -c 120 /dev/zero | tr '\000' "$(printf '%x' $((i % 16)))"; } |
                    head -c 120 >"$work/s$i"
                set +e
                MSYS_NO_PATHCONV=1 "$HARNESS" stream "$image" "$1" "s$i" --content "$(cat "$work/s$i")" \
                    >/dev/null 2>"$image.cli-err"
                status=$?
                set -e
                [ "$status" -eq 0 ] || die "rust-ntfs stream $1 s$i exited $status: $(cat "$image.cli-err")"
            done
        }
        # streams_kept FILE: every named stream still holds what was written.
        streams_kept() {
            local i
            for i in $(seq 1 "$streams"); do
                run fs "$image" read --stream "s$i" "$1"
                [ "$status" -eq 0 ] || die "fs.ntfs read --stream s$i $1 exited $status: $(cat "$image.cli-err")"
                [ "$out" = "$(cat "$work/s$i")" ] || die "$1:s$i changed under the unnamed stream's writes: $out"
            done
        }
        # same FILE: the unnamed stream reads back as the host's model of it.
        same() {
            set +e
            "$BIN" fs "$image" read "$1" >"$work/back" 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs read $1 exited $status: $(cat "$image.cli-err")"
            cmp -s "$work/back" "$work/model" || die "$1 does not read back as written"
            streams_kept "$1"
        }
        # rewritten FILE BYTES: the whole unnamed stream is replaced.
        rewritten() {
            random "$work/model" "$2"
            set +e
            out="$("$BIN" fs "$image" write "$1" <"$work/model" 2>"$image.cli-err")"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write $1 exited $status: $(cat "$image.cli-err")"
            same "$1"
        }
        # patched FILE AT BYTES: BYTES random bytes are written at AT.
        patched() {
            local size
            random "$work/patch" "$3"
            dd if="$work/patch" of="$work/model" bs=1 seek="$2" conv=notrunc 2>/dev/null
            size="$(wc -c <"$work/model" | tr -d ' ')"
            set +e
            out="$("$BIN" fs "$image" write --at "$2" "$1" <"$work/patch" 2>"$image.cli-err")"
            status=$?
            set -e
            [ "$status" -eq 0 ] && has "\"size\": $size" && has "\"at\": $2" && has "\"bytes\": $3" ||
                die "fs.ntfs write --at $2 $1: $out $(cat "$image.cli-err")"
            same "$1"
        }
        # resized FILE SIZE: the unnamed stream is truncated or zero-extended.
        resized() {
            local have
            have="$(wc -c <"$work/model" | tr -d ' ')"
            if [ "$2" -le "$have" ]; then
                head -c "$2" "$work/model" >"$work/next"
            else
                { cat "$work/model"; head -c $(($2 - have)) /dev/zero; } >"$work/next"
            fi
            cat "$work/next" >"$work/model"
            run fs "$image" truncate "$1" "$2"
            [ "$status" -eq 0 ] && has "\"size\": $2" || die "fs.ntfs truncate $1 $2: $out $(cat "$image.cli-err")"
            same "$1"
        }

        # /early.dat: its unnamed stream is non-resident before its record
        # fills, so it stays in the base record. /late.dat: its stream is
        # eight resident bytes when the record fills and grows only after,
        # so it has to be placed through the list.
        random "$work/model" 6000
        run fs "$image" write /early.dat <"$work/model"
        [ "$status" -eq 0 ] || die "fs.ntfs write /early.dat exited $status: $(cat "$image.cli-err")"
        listed /early.dat
        same /early.dat
        printf 'contents' >"$work/model"
        run fs "$image" write /late.dat <"$work/model"
        [ "$status" -eq 0 ] || die "fs.ntfs write /late.dat exited $status: $(cat "$image.cli-err")"
        listed /late.dat
        same /late.dat
        for file in /early.dat /late.dat; do
            rewritten "$file" 9000
            patched "$file" 4000 300
            patched "$file" 8990 20
            patched "$file" 12000 100
            resized "$file" 5000
            resized "$file" 7000
            resized "$file" 300
            patched "$file" 290 5000
            size="$(wc -c <"$work/model" | tr -d ' ')"
            printf 'file\t%s\t%s\t%s\n' "$file" "$size" "$(sha "$work/model")" >>"$manifest"
        done
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the unnamed stream's writes exited $status: $out"
        run fs "$image" get dirty --text
        [ "$status" -eq 0 ] && [ "$out" = false ] || die "the unnamed stream's writes left the volume dirty"
        ;;
    attrlist-split-extents)
        [ $# -eq 0 ] || die "attrlist-split-extents IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        HARNESS="${RUST_NTFS:-$REPO/target/release/rust-ntfs}"
        [ -x "$HARNESS" ] || [ -x "$HARNESS.exe" ] ||
            die "no $HARNESS: build it with \`cargo build --release --locked --features harness --bin rust-ntfs\`"
        cluster="$(cluster_size "$image")"
        # A one-cluster file has to be non-resident to leave a hole behind.
        [ "$cluster" -ge "$(record_size "$image")" ] ||
            die "$cluster-byte clusters: a one-cluster file would stay resident and leave no hole"
        # One-cluster holes the free space is cut into, and the stream's
        # length in clusters: one run per hole, many more mapping pairs than
        # one MFT record holds, fewer than there are holes (#799).
        holes=900
        clusters=800
        # A one-cluster run two clusters from the last takes at least three
        # bytes of mapping pairs, so the run list outgrows the record.
        [ $((clusters * 3)) -gt "$(record_size "$image")" ] ||
            die "$clusters runs fit in one $(record_size "$image")-byte record: the run list would not split"
        # extents: the unnamed $DATA extents /split.dat's list names.
        extents() {
            cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" --example listed_data_extents -- \
                "$image" /split.dat >"$work/extents" || die "listed_data_extents $image /split.dat failed"
            wc -l <"$work/extents" | tr -d ' '
        }
        # streams_kept: every named stream still holds what was written.
        streams_kept() {
            local i
            for i in $(seq 1 12); do
                run fs "$image" read --stream "s$i" /split.dat
                [ "$status" -eq 0 ] || die "fs.ntfs read --stream s$i /split.dat exited $status: $(cat "$image.cli-err")"
                [ "$out" = "$(cat "$work/s$i")" ] || die "/split.dat:s$i changed under the unnamed stream's writes: $out"
            done
        }
        # same: the unnamed stream reads back as the host's model of it,
        # and the list still names more than one extent of it.
        same() {
            set +e
            "$BIN" fs "$image" read /split.dat >"$work/back" 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs read /split.dat exited $status: $(cat "$image.cli-err")"
            cmp -s "$work/back" "$work/model" || die "/split.dat does not read back as written ($1)"
            streams_kept
            n="$(extents)"
            [ "$n" -gt 1 ] || die "/split.dat's list names $n unnamed \$DATA extents after $1, not a split run list"
        }
        # write_at AT BYTES: BYTES random bytes are written at AT.
        write_at() {
            random "$work/patch" "$2"
            dd if="$work/patch" of="$work/model" bs=1 seek="$1" conv=notrunc 2>/dev/null
            set +e
            out="$("$BIN" fs "$image" write --at "$1" /split.dat <"$work/patch" 2>"$image.cli-err")"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write --at $1 /split.dat exited $status: $(cat "$image.cli-err")"
            same "write --at $1"
        }
        # resized SIZE: the unnamed stream is truncated or zero-extended.
        resized() {
            local have
            have="$(wc -c <"$work/model" | tr -d ' ')"
            if [ "$1" -le "$have" ]; then
                head -c "$1" "$work/model" >"$work/next"
            else
                { cat "$work/model"; head -c $(($1 - have)) /dev/zero; } >"$work/next"
            fi
            cat "$work/next" >"$work/model"
            run fs "$image" truncate /split.dat "$1"
            [ "$status" -eq 0 ] && has "\"size\": $1" || die "fs.ntfs truncate /split.dat $1: $out $(cat "$image.cli-err")"
            same "truncate $1"
        }

        # /split.dat: eight resident bytes, a 64-byte EA and twelve 120-byte
        # named streams, so its base record fills and it gains a list (#663).
        printf 'contents' >"$work/model"
        run fs "$image" write /split.dat <"$work/model"
        [ "$status" -eq 0 ] || die "fs.ntfs write /split.dat exited $status: $(cat "$image.cli-err")"
        set +e
        "$HARNESS" ea "$image" /split.dat USER.TAG --content "$(printf 'e%.0s' $(seq 1 64))" \
            >/dev/null 2>"$image.cli-err"
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "rust-ntfs ea /split.dat USER.TAG exited $status: $(cat "$image.cli-err")"
        for i in $(seq 1 12); do
            { printf 's%02d of /split.dat ' "$i"; head -c 120 /dev/zero | tr '\000' "$(printf '%x' $((i % 16)))"; } |
                head -c 120 >"$work/s$i"
            set +e
            "$HARNESS" stream "$image" /split.dat "s$i" --content "$(cat "$work/s$i")" >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "rust-ntfs stream /split.dat s$i exited $status: $(cat "$image.cli-err")"
        done
        set +e
        cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" --example listed_data_extents -- \
            "$image" /split.dat >/dev/null
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "/split.dat's base record never filled: it has no \$ATTRIBUTE_LIST (exit $status)"

        # Cut the free space into one-cluster holes: 2 * holes one-cluster
        # files, a filler over all that is left but a few clusters, then
        # every second one-cluster file removed.
        run fs "$image" mkdir /c
        [ "$status" -eq 0 ] || die "fs.ntfs mkdir /c exited $status: $(cat "$image.cli-err")"
        head -c "$cluster" /dev/zero | tr '\000' '\305' >"$work/one"
        for i in $(seq 1 $((2 * holes))); do
            run fs "$image" write "/c/$i" <"$work/one"
            [ "$status" -eq 0 ] || die "fs.ntfs write /c/$i exited $status: $(cat "$image.cli-err")"
        done
        run fs "$image" get free_bytes --text
        [ "$status" -eq 0 ] || die "fs.ntfs get free_bytes exited $status: $(cat "$image.cli-err")"
        # What is left after the filler is less than the stream, so the
        # stream cannot sit in one run there.
        spare=$((clusters / 4))
        [ "$((out / cluster))" -gt "$spare" ] || die "only $out bytes free after the one-cluster files"
        head -c $((out - spare * cluster)) /dev/zero | tr '\000' '\074' >"$work/fill"
        run fs "$image" write /fill <"$work/fill"
        [ "$status" -eq 0 ] || die "fs.ntfs write /fill exited $status: $(cat "$image.cli-err")"
        for i in $(seq 2 2 $((2 * holes))); do
            run fs "$image" rm "/c/$i"
            [ "$status" -eq 0 ] || die "fs.ntfs rm /c/$i exited $status: $(cat "$image.cli-err")"
        done

        # The stream grows past its end over the holes, then is patched
        # inside, rewritten whole, cut and extended again (#800).
        size=$((clusters * cluster))
        write_at 8 $((size - 8))
        write_at $((size / 3)) $((5 * cluster + 7))
        random "$work/model" $((size - 3 * cluster))
        set +e
        out="$("$BIN" fs "$image" write /split.dat <"$work/model" 2>"$image.cli-err")"
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "fs.ntfs write /split.dat exited $status: $(cat "$image.cli-err")"
        same "the rewrite"
        resized $((size / 2 + 100))
        resized $((size - 2 * cluster))
        printf 'file\t/split.dat\t%s\t%s\n' "$(wc -c <"$work/model" | tr -d ' ')" "$(sha "$work/model")" >>"$manifest"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the split stream's writes exited $status: $out"
        run fs "$image" get dirty --text
        [ "$status" -eq 0 ] && [ "$out" = false ] || die "the split stream's writes left the volume dirty"
        ;;
    write-at)
        [ $# -eq 0 ] || die "write-at IMAGE"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$image.manifest"
        # Windows independently repeats each positioned write on a reference
        # containing OLD bytes of Z, with COUNT bytes of Q as the patch.
        for spec in 'resident 5 2 3' 'promoted 5 4095 3' \
            'inside 12289 20 3' 'boundary 12289 4095 3' \
            'append 12289 12289 3' 'gap 12289 16385 8193' \
            'empty 0 0 3' 'null 12289 16385 0'; do
            read -r name old at count <<<"$spec"
            head -c "$old" /dev/zero | tr '\000' Z >"$work/input"
            head -c "$count" /dev/zero | tr '\000' Q >"$work/patch"
            run fs "$image" write "/$name" <"$work/input"
            [ "$status" -eq 0 ] || die "write /$name: $(cat "$image.cli-err")"
            size="$old"
            if [ "$count" -gt 0 ] && [ "$((at + count))" -gt "$size" ]; then size=$((at + count)); fi
            run fs "$image" write --at "$at" "/$name" <"$work/patch"
            if [ "$status" -ne 0 ] || ! has "\"size\": $size" || \
                ! has "\"at\": $at" || ! has "\"bytes\": $count"; then
                die "write --at $at /$name: $out $(cat "$image.cli-err")"
            fi
            printf 'write-at\t/%s\t%s\t%s\t%s\n' "$name" "$old" "$at" "$count" >>"$image.manifest"
        done
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after positioned writes: $out"
        run fs "$image" get dirty --text
        [ "$status" -eq 0 ] && [ "$out" = false ] || die "positioned write left the volume dirty"
        ;;
    truncate)
        [ $# -eq 0 ] || die "truncate IMAGE"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$image.manifest"
        # NAME OLD FINAL [INTERMEDIATE]: the prefix is Z, with tails
        # independently supplied by Windows' FileStream.SetLength oracle.
        for spec in 'zero 12289 0' 'boundary 12289 4097' \
            'grown 4097 8193' 'regrown 12289 8193 4097' \
            'resident 5 8193' 'resident-shrink 5 2' 'empty 0 4097' \
            'same 4097 4097'; do
            read -r name old final middle <<<"$spec"
            if [ "$old" -eq 0 ]; then : >"$work/input";
            else head -c "$old" /dev/zero | tr '\000' Z >"$work/input"; fi
            run fs "$image" write "/$name" <"$work/input"
            [ "$status" -eq 0 ] || die "write /$name: $(cat "$image.cli-err")"
            if [ -n "$middle" ]; then
                run fs "$image" truncate "/$name" "$middle"
                [ "$status" -eq 0 ] || die "intermediate truncate /$name: $(cat "$image.cli-err")"
            fi
            run fs "$image" truncate "/$name" "$final"
            if [ "$status" -ne 0 ] || ! has "\"size\": $final"; then
                die "truncate /$name: $out $(cat "$image.cli-err")"
            fi
            printf 'truncate\t/%s\t%s\t%s\t%s\n' "$name" "$old" "$final" "$middle" >>"$image.manifest"
        done
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after truncate: $out"
        run fs "$image" get dirty --text
        [ "$status" -eq 0 ] && [ "$out" = false ] || die "truncate left the volume dirty"
        ;;
    mkdir-parents)
        [ $# -eq 0 ] || die "mkdir-parents IMAGE"
        manifest="$image.manifest"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        : >"$manifest"
        # made PATH STORED...: fs.ntfs mkdir -p PATH makes exactly the
        # directories STORED, in order, and says so. Each STORED is spelled
        # as the volume now spells it, which is how Windows must list it.
        # The report names each as PATH spelled it, `.` and `..` resolved:
        # the same path but for case, which is all --text is compared on.
        made() {
            local path="$1" dir want=""
            shift
            for dir in "$@"; do want="${want}created ${dir}"$'\n'; done
            run fs "$image" mkdir -p --text "$path"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir -p $path exited $status: $(cat "$image.cli-err")"
            [ "$(printf '%s' "$out" | tr '[:lower:]' '[:upper:]')" = "$(printf '%s' "${want%$'\n'}" | tr '[:lower:]' '[:upper:]')" ] ||
                die "fs.ntfs mkdir -p $path reported '$out', not the chain '${want%$'\n'}'"
            for dir in "$@"; do printf 'dir\t%s\nname\t%s\n' "$dir" "$dir" >>"$manifest"; done
        }
        # there PATH: fs.ntfs mkdir -p PATH finds the chain whole and writes nothing.
        there() {
            local before
            before="$(sha "$image")"
            run fs "$image" mkdir -p --text "$1"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir -p $1 exited $status: $(cat "$image.cli-err")"
            [ "$out" = "$1 exists; nothing created" ] || die "fs.ntfs mkdir -p $1 reported: $out"
            [ "$(sha "$image")" = "$before" ] || die "fs.ntfs mkdir -p $1, already there, wrote to the volume"
        }
        # refused PATTERN PATH: fs.ntfs mkdir -p PATH exits 1 naming PATTERN
        # and writes nothing, so no part of the chain is left behind.
        refused() {
            local before
            before="$(sha "$image")"
            run fs "$image" mkdir -p "$2"
            [ "$status" -eq 1 ] && grep -qi -- "$1" "$image.cli-err" ||
                die "fs.ntfs mkdir -p $2 exited $status, not 1 for '$1': $(cat "$image.cli-err")"
            [ "$(sha "$image")" = "$before" ] || die "the refused fs.ntfs mkdir -p $2 wrote to the volume"
        }

        made /Chain/One/Two /Chain /Chain/One /Chain/One/Two
        made /CHAIN/one/Two/three/four /Chain/One/Two/three /Chain/One/Two/three/four
        made /Chain/./x/../dotted /Chain/dotted
        made /deep/1/2/3/4/5/6/7/8/9/10/11 /deep /deep/1 /deep/1/2 /deep/1/2/3 /deep/1/2/3/4 \
            /deep/1/2/3/4/5 /deep/1/2/3/4/5/6 /deep/1/2/3/4/5/6/7 /deep/1/2/3/4/5/6/7/8 \
            /deep/1/2/3/4/5/6/7/8/9 /deep/1/2/3/4/5/6/7/8/9/10 /deep/1/2/3/4/5/6/7/8/9/10/11
        u="/na$(printf '\303\257')ve/$(printf '\346\227\245\346\234\254')"
        made "$u/$(printf '\316\251')" "${u%/*}" "$u" "$u/$(printf '\316\251')"
        # 300 names of varying length, each the start of a two-level chain:
        # /wide fills past one index level, so later chains go into index
        # blocks rather than the root.
        made /wide /wide
        for i in $(seq 1 300); do
            name="$(printf 'chain-%03d-%s' "$i" "$(printf '%*s' $((i % 23)) '' | tr ' ' x)")"
            made "/wide/$name/leaf" "/wide/$name" "/wide/$name/leaf"
        done

        there /Chain/One/Two
        there /CHAIN/ONE/TWO/
        there /deep/1/./2/../2

        # A file in the way, on the way and at PATH, is refused, and Windows
        # must refuse the same chains; names Windows cannot address and a
        # metafile's directory are refused before the first directory is made.
        run fs "$image" mkdir /files
        [ "$status" -eq 0 ] || die "fs.ntfs mkdir /files exited $status: $(cat "$image.cli-err")"
        random "$work/f" 500
        set +e
        "$BIN" fs "$image" write /files/f.txt <"$work/f" >/dev/null 2>"$image.cli-err"
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "fs.ntfs write /files/f.txt exited $status: $(cat "$image.cli-err")"
        for path in /files/f.txt/x/y /FILES/F.TXT/x /files/f.txt; do
            case "$path" in
                */x*) refused 'not a directory' "$path" ;;
                *) refused 'already exists' "$path" ;;
            esac
            printf 'refuse\t%s\n' "$path" >>"$manifest"
        done
        refused 'cannot address' '/fresh/a:b/c'
        refused 'trailing space or period' /fresh/x./y
        # shellcheck disable=SC2016 # \$Extend is the metafile's name, not a variable.
        refused 'metafile' '/$Extend/fresh'

        printf 'file\t/files/f.txt\t500\t%s\n' "$(sha "$work/f")" >>"$manifest"
        printf 'absent\t%s\n' /Chain/x /fresh >>"$manifest"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the chains exited $status: $out"
        ;;
    replay)
        [ $# -eq 1 ] || die "replay IMAGE SNAPSHOT"
        unpack_interrupted "$1"
        run fsck -y "$image"
        [ "$status" -eq 1 ] || die "fsck.ntfs -y exited $status, not 1 (corrected): $out"
        has '"logfile": "empty"' || die "fsck.ntfs -y did not leave the log empty: $out"
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the replay exited $status: $out"
        windows_listing "$1"
        ;;
    replay-stopped)
        [ $# -eq 1 ] || die "replay-stopped IMAGE SNAPSHOT"
        unpack_interrupted "$1"
        # A power cut right after the replay's first undo write (#754): the
        # blocks written so far stay, and the log still holds work, so the
        # next mount -- Windows, in the scenario -- runs its own restart
        # over what the stopped replay left (#648).
        set +e
        out="$(cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" \
            --example replay_stopped -- "$image" 2>&1)"
        status=$?
        set -e
        [ "$status" -eq 0 ] || die "replay_stopped exited $status: $out"
        run fsck "$image"
        [ "$status" -eq 4 ] || die "fsck.ntfs after the stopped replay exited $status, not 4: $out"
        has '"kind": "logfile"' || die "fsck.ntfs did not report the log holding work after the stop: $out"
        windows_listing "$1"
        ;;
    undo-index-compare)
        [ $# -eq 1 ] || die "undo-index-compare IMAGE SNAPSHOT"
        # IMAGE is what Windows' restart left over a stopped replay (#755);
        # each index block the replay's undo writes is compared with the
        # same block after an uninterrupted replay of the untouched capture
        # (#756), which a listing and chkdsk /scan can both miss (#514).
        restarted="$image"
        image="$image.uninterrupted"
        unpack_interrupted "$1"
        set +e
        out="$(cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" \
            --example undo_index_compare -- "$image" "$restarted" 2>&1)"
        status=$?
        set -e
        rm -f "$image"
        printf '%s\n' "$out"
        [ "$status" -eq 0 ] || die "undo_index_compare exited $status"
        ;;
    replay-lsn)
        [ $# -eq 1 ] || die "replay-lsn IMAGE SNAPSHOT"
        unpack_interrupted "$1"
        run fsck -y "$image"
        [ "$status" -eq 1 ] || die "fsck.ntfs -y exited $status, not 1 (corrected): $out"
        has '"logfile": "empty"' || die "fsck.ntfs -y did not leave the log empty: $out"
        # The LSNs the replay left on the volume (#649): its MFT records and
        # index blocks keep those of the old log, which the replay filled
        # with 0xFF. Read from the raw bytes, for lsn-after-windows to
        # compare with what Windows logs once it has written.
        lsns "$image" >"$image.lsns"
        grep -q '^block-max ' "$image.lsns" || die "no FILE or INDX block carries an LSN: $(cat "$image.lsns")"
        echo "replay-lsn: the replay left: $(tr '\n' ' ' <"$image.lsns")"
        ;;
    lsn-after-windows)
        [ $# -eq 0 ] || die "lsn-after-windows IMAGE"
        [ -s "$image.lsns" ] || die "no $image.lsns: run replay-lsn first"
        # Redo skips a block whose LSN is at or past a record's (#649): if
        # Windows' fresh log starts below the LSNs the replay left, its own
        # restart after the next unclean shutdown skips its records for
        # those blocks. Every restart area Windows wrote -- one not on the
        # volume before it was attached -- must name a current LSN past the
        # highest the replay left.
        lsns "$image" >"$image.lsns-after"
        echo "lsn-after-windows: Windows left: $(tr '\n' ' ' <"$image.lsns-after")"
        floor="$(awk '$1 == "block-max" { print $2 }' "$image.lsns")"
        new="$(grep '^restart-current ' "$image.lsns-after" | grep -vxF -f "$image.lsns" || true)"
        [ -n "$new" ] || die "Windows wrote no restart area the replayed volume did not already hold"
        old="$(printf '%s\n' "$new" | awk -v f="$floor" '$2 <= f { print $2 }')"
        [ -z "$old" ] || die "Windows' restart areas name current LSN(s) $(echo $old) at or below $floor, the highest the replay left on the volume (#649)"
        echo "lsn-after-windows: every restart area Windows wrote is past $floor"
        ;;
    write-after-replay)
        [ $# -eq 1 ] || die "write-after-replay IMAGE SNAPSHOT"
        unpack_interrupted "$1"
        windows_listing "$1"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        # A write is refused while the log holds work (#137): Windows leaves
        # the same log after a Fast Startup shutdown, when replaying it
        # corrupts the volume, so the replay is fsck.ntfs -y's, asked for by
        # someone who knows Windows shut down fully. The refusal writes
        # nothing.
        before="$(sha "$image")"
        run fs "$image" mkdir /after-replay
        [ "$status" -eq 1 ] || die "fs.ntfs mkdir over a log holding work exited $status, not 1 (refused): $(cat "$image.cli-err")"
        grep -q 'LogFile' "$image.cli-err" && grep -q 'fsck.ntfs -y' "$image.cli-err" ||
            die "the refusal names neither \$LogFile nor fsck.ntfs -y: $(cat "$image.cli-err")"
        [ "$(sha "$image")" = "$before" ] || die "the refused fs.ntfs mkdir wrote to the volume"
        run fsck -y "$image"
        [ "$status" -eq 1 ] || die "fsck.ntfs -y exited $status, not 1 (corrected): $out"
        # Then a directory, a non-resident file in it, a resident file in a
        # directory the log changed, and a file the log created, replaced.
        # Growing $MFT on the same volume is the grow-mft step's.
        replaced="$(awk -F '\t' '$2 ~ /^\/d000\// { print $2; exit }' "$image.manifest")"
        [ -n "$replaced" ] || die "Windows' listing has no file under /d000"
        run fs "$image" mkdir /after-replay
        [ "$status" -eq 0 ] || die "fs.ntfs mkdir after the replay exited $status: $(cat "$image.cli-err")"
        printf 'dir\t/after-replay\n' >>"$image.manifest"
        for spec in /after-replay/new.bin:5000 /d000/after-replay.txt:500 "$replaced:4097"; do
            path="${spec%:*}"
            size="${spec##*:}"
            random "$work/data" "$size"
            set +e
            "$BIN" fs "$image" write "$path" <"$work/data" >/dev/null 2>"$image.cli-err"
            status=$?
            set -e
            [ "$status" -eq 0 ] || die "fs.ntfs write $path ($size bytes) exited $status: $(cat "$image.cli-err")"
            grep -v "^file	$path	" "$image.manifest" >"$work/m" || true
            printf 'file\t%s\t%s\t%s\n' "$path" "$size" "$(sha "$work/data")" >>"$work/m"
            cp "$work/m" "$image.manifest"
        done
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the writes exited $status: $out"
        has '"logfile": "empty"' || die "the log is not empty after the writes: $out"
        ;;
    grow-mft)
        [ $# -eq 1 ] || die "grow-mft IMAGE SNAPSHOT"
        mkdir -p "$(dirname "$image")"
        gzip -dc "$REPO/test-disks/windows-interrupted-$1.recovered.img.gz" >"$image" ||
            die "no test-disks/windows-interrupted-$1.recovered.img.gz"
        windows_listing "$1"
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        # How many records $MFT holds, as fs.ntfs info reports it. Only a
        # loop bound: Windows grades what was written, and the crate's own
        # test reads the count from $MFT's run list.
        records() {
            run fs "$image" info
            [ "$status" -eq 0 ] || die "fs.ntfs info exited $status: $(cat "$image.cli-err")"
            sed -n 's/.*"mft_total_records": \([0-9]*\).*/\1/p' <<<"$out"
        }
        start="$(records)"
        [ -n "$start" ] || die "fs.ntfs info reported no mft_total_records: $out"
        run fs "$image" mkdir /grown
        [ "$status" -eq 0 ] || die "fs.ntfs mkdir /grown exited $status: $(cat "$image.cli-err")"
        printf 'dir\t/grown\n' >>"$image.manifest"
        # Every record $MFT holds is used, then two growths of 64 more.
        # Each 32-file batch checks how far $MFT has come; the cap is far
        # past what that needs, so a $MFT that never grows fails here.
        # Each batch has a directory of its own: 32 names fit one index
        # block, so the directories stay one level deep and what is graded
        # is $MFT's growth, not a deep index B-tree (rust-fs-ntfs#432).
        i=0
        while [ "$(records)" -le $((start + 64)) ]; do
            [ "$i" -lt 2048 ] || die "$i files written and \$MFT still holds $(records) records, $start at the start"
            batch="$(printf '/grown/b%03d' $((i / 32)))"
            run fs "$image" mkdir "$batch"
            [ "$status" -eq 0 ] || die "fs.ntfs mkdir $batch exited $status: $(cat "$image.cli-err")"
            printf 'dir\t%s\n' "$batch" >>"$image.manifest"
            for _ in $(seq 32); do
                path="$(printf '%s/f%04d.bin' "$batch" "$i")"
                size=$((300 + i))
                random "$work/data" "$size"
                set +e
                "$BIN" fs "$image" write "$path" <"$work/data" >/dev/null 2>"$image.cli-err"
                status=$?
                set -e
                [ "$status" -eq 0 ] || die "fs.ntfs write $path ($size bytes, file $i) exited $status: $(cat "$image.cli-err")"
                printf 'file\t%s\t%s\t%s\n' "$path" "$size" "$(sha "$work/data")" >>"$image.manifest"
                i=$((i + 1))
            done
        done
        run fsck "$image"
        [ "$status" -eq 0 ] || die "fsck.ntfs after the writes exited $status: $out"
        ;;
    *)
        die "unknown step '$step'"
        ;;
esac
rm -f "$image.cli-err"
echo "cli-oracle: $step $image: ok"
