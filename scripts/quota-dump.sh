#!/usr/bin/env bash
# quota-dump.sh IMAGE NAME PATH... -- what a volume's $Extend\$Quota holds,
# read by this crate's raw reader (examples/quota_dump.rs), for a matrix
# scenario that measures what Windows wrote there (#1085). One row per $O
# entry (owner SID and id), per $Q record (owner id, flags, usage and SID)
# and per PATH (the owner id and charge in its $STANDARD_INFORMATION). The
# rows go to stdout and to test-diagnostics/matrix/NAME.quota.tsv, which
# the matrix job keeps.
set -euo pipefail

# Paths inside the image are not host paths: Git Bash must not rewrite them.
export MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[ $# -ge 3 ] || { echo "usage: quota-dump.sh IMAGE NAME PATH..." >&2; exit 2; }
image="$1"
name="$2"
shift 2

out="$REPO/test-diagnostics/matrix/$name.quota.tsv"
mkdir -p "$(dirname "$out")"
cargo run --quiet --release --manifest-path "$REPO/Cargo.toml" --example quota_dump -- "$image" "$@" >"$out"
cat "$out"
for path in "$@"; do
    grep -q "^file	$path	[0-9]" "$out" || { echo "quota-dump: $path has no owner id in the dump" >&2; exit 1; }
done
