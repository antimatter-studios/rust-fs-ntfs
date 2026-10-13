# tests/cli/lib-ntfs.sh -- reading and damaging an NTFS image from the shell,
# by the on-disk layout (MS-FSCC and the boot sector), not through this
# crate: a test that corrupts a volume must not ask the code under test
# where to aim. Sourced after lib.sh. Little-endian hosts only, which is
# every platform the tools ship for.

# u8/u16/u64 IMAGE OFFSET: an unsigned little-endian field.
u8() { od -An -tu1 -j "$2" -N 1 "$1" | tr -d ' \n'; }
u16() { od -An -tu2 -j "$2" -N 2 "$1" | tr -d ' \n'; }
u64() { od -An -tu8 -j "$2" -N 8 "$1" | tr -d ' \n'; }

# cluster_size IMAGE: bytes per sector (+0x0B) times sectors per cluster (+0x0D).
cluster_size() { echo $(($(u16 "$1" 11) * $(u8 "$1" 13))); }

# record_size IMAGE: clusters per MFT record (+0x40); a negative value n
# means 2^-n bytes.
record_size() {
    local n
    n="$(u8 "$1" 64)"
    if [ "$n" -lt 128 ]; then echo $((n * $(cluster_size "$1"))); else echo $((1 << (256 - n))); fi
}

# record_offset IMAGE N: where MFT record N starts, for a volume whose $MFT
# is one run from its first LCN (+0x30) -- what mkfs.ntfs writes.
record_offset() { echo $(($(u64 "$1" 48) * $(cluster_size "$1") + $2 * $(record_size "$1"))); }

# mirror_offset IMAGE N: where $MFTMirr's copy of record N starts (+0x38).
mirror_offset() { echo $(($(u64 "$1" 56) * $(cluster_size "$1") + $2 * $(record_size "$1"))); }

# poke IMAGE OFFSET BYTES: overwrite bytes in place (printf escapes allowed).
poke() { printf "$3" | dd of="$1" bs=1 seek="$2" conv=notrunc 2>/dev/null; }

# byte_at IMAGE OFFSET: the byte there, as two hex digits.
byte_at() { od -An -tx1 -j "$2" -N1 "$1" | tr -d ' \n'; }

# flip IMAGE OFFSET: invert every bit of the byte there, so it changes
# whatever it held. A constant written over a byte that already held it
# changes nothing, which a timestamp's byte does one time in 256 (#558).
flip() {
    local was
    was=$(byte_at "$1" "$2")
    poke "$1" "$2" "$(printf '\\x%02x' $((0x$was ^ 0xff)))"
}
