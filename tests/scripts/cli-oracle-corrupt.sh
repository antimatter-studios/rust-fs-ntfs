#!/usr/bin/env bash
# `cli-oracle.sh corrupt IMAGE mirror|signature` must change the image
# whatever its bytes were (#1361). The mirror byte it damages is the low byte
# of a 100 ns timestamp, so a constant written there is already in place on
# about one format in 256: the mirror then still matched $MFT, and the
# matrix's fsck step correctly found nothing. Each operation here starts with
# the byte it targets already holding the old constant.
#
# corrupt reads only the boot sector's geometry, so a boot sector and two
# records' worth of zeros stand in for a formatted volume: no CLI is run.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
# corrupt never runs the CLI: a stub that fails says so if it does.
printf '#!/bin/sh\necho "corrupt ran the CLI: $*" >&2\nexit 99\n' >"$sandbox/ntfs"
chmod +x "$sandbox/ntfs"
export RUST_FS_NTFS="$sandbox/ntfs"
fails=0
checks=0
fail() { echo "FAIL  $*" >&2; fails=$((fails + 1)); }
check() { checks=$((checks + 1)); }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
byte_at() { od -An -tx1 -j "$2" -N "${3:-1}" "$1" | tr -d ' \n'; }
poke() { printf "$3" | dd of="$1" bs=1 seek="$2" conv=notrunc 2>/dev/null; }
le64() { python3 -c 'import struct,sys; sys.stdout.buffer.write(struct.pack("<Q", int(sys.argv[1])))' "$1"; }

# 512-byte sectors, 8 per cluster (4096), 1024-byte records ($F6),
# $MFT at cluster 4 and $MFTMirr at cluster 2.
CLUSTER=4096
RECORD=1024
MFT=4
MIRROR=2
image="$sandbox/volume.img"
head -c $(((MFT + 4) * CLUSTER)) /dev/zero >"$image"
poke "$image" 11 '\x00\x02'
poke "$image" 13 '\x08'
le64 "$MFT" | dd of="$image" bs=1 seek=48 conv=notrunc 2>/dev/null
le64 "$MIRROR" | dd of="$image" bs=1 seek=56 conv=notrunc 2>/dev/null
poke "$image" 64 '\xf6'

# corrupt OPERATION OFFSET LENGTH PRESET: OFFSET already holds PRESET, the
# value the old code wrote; corrupt must change the image and those bytes.
corrupt() {
    local operation="$1" offset="$2" length="$3" preset="$4"
    poke "$image" "$offset" "$preset"
    local before after was
    before="$(sha "$image")"
    was="$(byte_at "$image" "$offset" "$length")"
    if ! bash "$ROOT/scripts/cli-oracle.sh" corrupt "$image" "$operation" >"$sandbox/log" 2>&1; then
        fail "corrupt $operation exited non-zero: $(cat "$sandbox/log")"
        return
    fi
    after="$(sha "$image")"
    check
    [ "$before" != "$after" ] || fail "corrupt $operation left the image byte-identical (bytes were already $was)"
    check
    [ "$(byte_at "$image" "$offset" "$length")" != "$was" ] ||
        fail "corrupt $operation did not change the bytes at $offset"
}

corrupt mirror $((MIRROR * CLUSTER + 200)) 1 '\x5a'
corrupt mirror $((MIRROR * CLUSTER + 200)) 1 '\x00'
corrupt signature $((MFT * CLUSTER + 13 * RECORD)) 4 'XXXX'
corrupt signature $((MFT * CLUSTER + 13 * RECORD)) 4 'FILE'

[ "$checks" -ge 8 ] || fail "only $checks checks ran, floor 8"
if [ "$fails" -gt 0 ]; then
    echo "cli-oracle-corrupt.sh: $fails of $checks checks failed"
    exit 1
fi
echo "cli-oracle-corrupt.sh: all $checks checks passed"
