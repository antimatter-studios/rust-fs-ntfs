#!/usr/bin/env bash
# Build the NTFS read-path test-disk fixtures (test-disks/ntfs-*.img).
#
# The fixture-driven integration tests (tests/ads.rs, tests/unicode.rs,
# tests/deep.rs, …) open pre-built NTFS images raw through the crate's own
# reader. Those images are intentionally NOT committed (large binaries,
# listed in .gitignore) — they are generated here.
#
# How it works
# ------------
# Each fixture is formatted with `mkntfs` and then populated by mounting it
# with `ntfs-3g` and copying known content in. `ntfs-3g` is used ONLY here,
# at fixture-creation time — the exact parallel to `mount -t ext4 -o loop`
# in rust-fs-ext4's builder. The test binary never touches it: `cargo test`
# opens the .img files raw through the fs-ntfs crate.
#
# Platform
# --------
# `mkntfs` + an `ntfs-3g` loop-mount are Linux-only. On a Linux host (incl.
# ubuntu CI runners) this runs the builders natively — no VM. The tools come
# from the `ntfs-3g` package:
#
#   apt-get install -y ntfs-3g attr  # Debian / Ubuntu  (mkntfs + ntfs-3g, setfattr)
#
# Mounting needs root, so the mount/umount steps run under `sudo` when the
# script is not already root.
#
# On macOS there is no native mkntfs/ntfs-3g; build the fixtures on a Linux
# box (or in CI) and copy them over, or run this inside a Linux container.
#
# Usage
#   bash test-disks/build-ntfs-feature-images.sh                 # all images
#   bash test-disks/build-ntfs-feature-images.sh basic ads       # named ones

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# ---------------------------------------------------------------------------
# Tool discovery. mkntfs + ntfs-3g must be on PATH (they live in /sbin on
# some distros, which isn't always on a non-login PATH).
# ---------------------------------------------------------------------------
export PATH="$PATH:/sbin:/usr/sbin"

if [ "$(uname -s)" != "Linux" ]; then
    echo "ERROR: this builder needs Linux (mkntfs + ntfs-3g loop-mount)." >&2
    echo "       Run it on a Linux host / container / CI runner." >&2
    exit 1
fi

for tool in mkntfs ntfs-3g truncate dd setfattr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "ERROR: '$tool' not found on PATH." >&2
        echo "       Install the fixture toolchain:  apt-get install -y ntfs-3g attr" >&2
        exit 1
    fi
done

# Root is required to loop-mount. Re-exec individual mount/umount under sudo
# when we are not already uid 0.
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    else
        echo "ERROR: must run as root (loop-mount) and 'sudo' is unavailable." >&2
        exit 1
    fi
fi

MNT="$(mktemp -d "${TMPDIR:-/tmp}/ntfs-fixture.XXXXXX")"
cleanup() {
    # Best-effort unmount on any exit path so a failed builder doesn't leave
    # a dangling FUSE mount behind.
    if mountpoint -q "$MNT" 2>/dev/null; then
        $SUDO umount "$MNT" 2>/dev/null || true
    fi
    rmdir "$MNT" 2>/dev/null || true
}
trap cleanup EXIT

mount_ntfs() {
    # streams_interface=windows lets us write alternate data streams via
    # path:stream syntax. big_writes improves throughput for large fixtures.
    $SUDO ntfs-3g -o streams_interface=windows,big_writes,default_permissions \
        "$1" "$MNT"
}

umount_ntfs() {
    sync
    $SUDO umount "$MNT"
    # ntfs-3g unmount is async; wait for the FUSE handle to drop before
    # touching the image again.
    while pgrep -x ntfs-3g >/dev/null 2>&1; do
        sleep 0.1
    done
}

# `as_user <file> <cmd...>` — run a populate command that writes through the
# mount. The mount is owned by root (we mounted under sudo), so writes must
# also go through sudo. We funnel everything through `sudo sh -c` so shell
# redirection lands on the root-owned mount.
run_root() {
    $SUDO sh -c "$1"
}

# --- image builders -------------------------------------------------------

build_basic() {
    local img=ntfs-basic.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 16M "$img"
    mkntfs -q -F -f -L "BasicNTFS" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    run_root "printf 'Hello from NTFS!\n' > '$MNT/hello.txt'"
    run_root "mkdir -p '$MNT/Documents'"
    run_root "printf 'Test document content\n' > '$MNT/Documents/readme.txt'"
    run_root "printf 'Some notes here.\n'       > '$MNT/Documents/notes.txt'"
    umount_ntfs
}

build_manyfiles() {
    local img=ntfs-manyfiles.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 32M "$img"
    mkntfs -q -F -f -L "ManyFiles" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    run_root "mkdir -p '$MNT/bigdir'
              i=1
              while [ \$i -le 512 ]; do
                  printf 'content of file %03d\n' \$i > '$MNT'/bigdir/file_\$i.txt
                  i=\$((i + 1))
              done
              printf 'control\n' > '$MNT/small.txt'"
    umount_ntfs
}

build_large_file() {
    local img=ntfs-large-file.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 64M "$img"
    mkntfs -q -F -f -L "LargeFile" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    # 8 MB of zeros, then stamp a distinct ASCII byte ('A'..'H') at the start
    # of each 1 MB block. Tests read those marker bytes to verify offset-
    # correct reads across a non-resident $DATA attribute.
    run_root "dd if=/dev/zero of='$MNT/big.bin' bs=1M count=8 status=none
              i=0
              while [ \$i -lt 8 ]; do
                  off=\$((i * 1048576))
                  byte=\$(printf '\\\\%03o' \$((0101 + i)))
                  printf \"\$byte\" | dd of='$MNT/big.bin' bs=1 count=1 seek=\$off conv=notrunc status=none
                  i=\$((i + 1))
              done
              printf 'small control file\n' > '$MNT/small.txt'"
    umount_ntfs
}

build_sparse() {
    local img=ntfs-sparse.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 32M "$img"
    mkntfs -q -F -f -L "Sparse" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    # 4 MB file with holes: 1 byte every 1 MB, truncated to 4 MB.
    run_root "dd if=/dev/zero of='$MNT/sparse.bin' bs=1 count=0 seek=4M status=none
              off=0
              while [ \$off -lt 4000000 ]; do
                  printf 'X' | dd of='$MNT/sparse.bin' bs=1 count=1 seek=\$off conv=notrunc status=none
                  off=\$((off + 1048576))
              done
              printf 'dense\n' > '$MNT/dense.txt'"
    umount_ntfs
}

build_ads() {
    local img=ntfs-ads.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 16M "$img"
    mkntfs -q -F -f -L "ADSTest" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    run_root "printf 'primary data\n' > '$MNT/tagged.txt'"
    # Named $DATA attributes via the streams_interface=windows path syntax.
    run_root "printf 'alice author stream\n' > '$MNT/tagged.txt:author'"
    run_root "printf 'one-line summary\n'    > '$MNT/tagged.txt:summary'"
    run_root "printf 'no streams here\n' > '$MNT/plain.txt'"
    umount_ntfs
}

build_unicode() {
    local img=ntfs-unicode.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 16M "$img"
    mkntfs -q -F -f -L "Unicode" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    # Filenames hit three UTF-16 ranges: BMP Latin (umlaut), CJK (Japanese),
    # and astral (emoji -> UTF-16 surrogate pair), plus a Cyrillic dir.
    run_root "printf 'umlaut\n'   > '$MNT/grüße.txt'
              printf 'japanese\n' > '$MNT/日本語.txt'
              printf 'emoji\n'    > '$MNT/hello-🌍.txt'
              mkdir -p '$MNT/папка'
              printf 'cyrillic dir\n' > '$MNT/папка/file.txt'"
    umount_ntfs
}

build_deep() {
    local img=ntfs-deep.img
    echo "[build] $img"
    rm -f "$img"
    truncate -s 16M "$img"
    mkntfs -q -F -f -L "Deep" -c 4096 --with-uuid "$img"
    mount_ntfs "$img"
    # 20-level nested path — exercises repeated directory-index walks.
    run_root "path='$MNT'
              i=1
              while [ \$i -le 20 ]; do
                  path=\$path/level\$i
                  mkdir -p \"\$path\"
                  i=\$((i + 1))
              done
              printf 'deep file content\n' > \"\$path/buried.txt\"
              printf 'surface\n' > '$MNT/surface.txt'"
    umount_ntfs
}

# A VOLUME ANOTHER IMPLEMENTATION POPULATED (#485). Every other image here
# holds a handful of files. One volume ntfs-3g filled the way a user's would
# be found four reader bugs at once (#468, #471, #472, #473): a directory
# whose $INDEX_ROOT moved to an extension record, streams in extension
# records, a $DATA run list split across records, and writes into a file
# that already had an $ATTRIBUTE_LIST. This builds that volume at a 512-byte
# and a 4 KiB cluster, and writes beside each a manifest of what the mount
# itself reports: every directory, every file's size and SHA-256, and every
# named stream's. tests/ntfs3g_populated.rs reads the volume back against it.
#
# Manifest lines, tab-separated, paths from the volume's root:
#   D  <path>
#   F  <path>  <size>  <sha256>
#   S  <path>  <stream>  <size>  <sha256>
build_populated() {
    local cluster="$1"
    local img="ntfs-populated-$cluster.img"
    local manifest="ntfs-populated-$cluster.manifest.tsv"
    echo "[build] $img"
    rm -f "$img" "$manifest"
    truncate -s 256M "$img"
    mkntfs -q -F -f -L "Populated" -c "$cluster" --with-uuid "$img"
    $SUDO ntfs-3g -o streams_interface=windows,big_writes,compression,default_permissions \
        "$img" "$MNT"
    local populate
    populate="$(mktemp "${TMPDIR:-/tmp}/ntfs-populate.XXXXXX")"
    cat >"$populate" <<'EOF'
set -eu
cd "$1"
# 600 names of growing length in one directory: a multi-level index.
mkdir wide
i=1
while [ "$i" -le 600 ]; do
    name="$(printf '%*s' $((i % 120 + 1)) '' | tr ' ' n)-$i"
    printf 'entry %d\n' "$i" >"wide/$name"
    i=$((i + 1))
done
# A compressed directory (FILE_ATTRIBUTE_COMPRESSED), whose files inherit
# it: text of 10 B to 1 MiB, which LZNT1 shrinks, and 300 KiB of random
# bytes, which it cannot.
mkdir compressed
setfattr -n system.ntfs_attrib_be -v 0x00000800 compressed
for n in 10 1000 10000 100000 1048576; do
    yes 'a line of text that compresses well' | head -c "$n" >"compressed/text-$n.txt"
done
head -c 307200 /dev/urandom >compressed/random.bin
# 60 long names for one file: its $FILE_NAME attributes spill into
# extension records, and the directory's $INDEX_ROOT follows.
mkdir links
printf 'one file, sixty names\n' >links/target
i=1
while [ "$i" -le 60 ]; do
    ln links/target "links/$(printf '%*s' 200 '' | tr ' ' l)-$i"
    i=$((i + 1))
done
# 40 named streams and a 90 KiB non-resident one on one file.
printf 'the unnamed stream\n' >streams.txt
i=1
while [ "$i" -le 40 ]; do
    printf 'stream %d of 40\n' "$i" >"streams.txt:s$i"
    i=$((i + 1))
done
head -c 92160 /dev/urandom >"streams.txt:big"
# A 50 MiB sparse file with two islands of data.
truncate -s 50M sparse.bin
printf 'the first island' | dd of=sparse.bin bs=1 seek=1048576 conv=notrunc status=none
printf 'the second island' | dd of=sparse.bin bs=1 seek=41943040 conv=notrunc status=none
# Two files grown in alternating 8 KiB chunks: fragmented run lists.
i=1
while [ "$i" -le 96 ]; do
    head -c 8192 /dev/urandom >>frag-a.bin
    head -c 8192 /dev/urandom >>frag-b.bin
    i=$((i + 1))
done
# Names across UTF-16, an astral one, one of 250 characters, a deep path
# and an empty file.
printf 'umlaut\n' >'grüße.txt'
printf 'japanese\n' >'日本語.txt'
printf 'emoji\n' >'hello-🌍.txt'
printf 'long\n' >"$(printf '%*s' 250 '' | tr ' ' L)"
mkdir -p deep/one/two/three/four/five
printf 'five levels down\n' >deep/one/two/three/four/five/file.txt
: >empty
EOF
    run_root "bash '$populate' '$MNT'"
    rm -f "$populate"

    local list
    list="$(mktemp "${TMPDIR:-/tmp}/ntfs-manifest.XXXXXX")"
    cat >"$list" <<'EOF'
set -eu
cd "$1"
find . -mindepth 1 -type d | LC_ALL=C sort | while IFS= read -r p; do
    printf 'D\t%s\n' "${p#.}"
done
find . -type f | LC_ALL=C sort | while IFS= read -r p; do
    printf 'F\t%s\t%s\t%s\n' "${p#.}" "$(stat -c %s "$p")" "$(sha256sum <"$p" | cut -d' ' -f1)"
done
for s in $(seq 1 40 | sed 's/^/s/') big; do
    printf 'S\t/streams.txt\t%s\t%s\t%s\n' "$s" "$(stat -c %s "streams.txt:$s")" \
        "$(sha256sum <"streams.txt:$s" | cut -d' ' -f1)"
done
EOF
    $SUDO bash "$list" "$MNT" >"$manifest"
    rm -f "$list"
    umount_ntfs
    echo "[build] $manifest: $(grep -c . "$manifest") entries"
}

# --- dispatch -------------------------------------------------------------

ALL="basic manyfiles large-file sparse ads unicode deep populated"
TARGETS="${*:-$ALL}"

for t in $TARGETS; do
    case "$t" in
        basic)      build_basic ;;
        manyfiles)  build_manyfiles ;;
        large-file) build_large_file ;;
        sparse)     build_sparse ;;
        ads)        build_ads ;;
        unicode)    build_unicode ;;
        deep)       build_deep ;;
        populated)  build_populated 512; build_populated 4096 ;;
        *)          echo "unknown target: $t (have: $ALL)" >&2; exit 1 ;;
    esac
done

echo "[build] done — fixtures in $SCRIPT_DIR"
ls -lh "$SCRIPT_DIR"/ntfs-*.img 2>/dev/null || true
