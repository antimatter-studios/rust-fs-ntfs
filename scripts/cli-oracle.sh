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
#   set-dirty IMAGE                  fs.ntfs set dirty true
#   corrupt IMAGE mirror|signature   damage the volume by its on-disk layout
#   fsck-finds IMAGE KIND            fsck.ntfs exits 4 and reports a finding of KIND
#
# The binary is target/release/rust-fs-ntfs (built with `--features cli`),
# or RUST_FS_NTFS. JSON is read with grep, not jq: the Windows runner's Git
# Bash is not promised to carry jq, and each check is one field.
set -euo pipefail

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
    *)
        die "unknown step '$step'"
        ;;
esac
rm -f "$image.cli-err"
echo "cli-oracle: $step $image: ok"
