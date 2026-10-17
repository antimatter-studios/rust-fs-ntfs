#!/usr/bin/env bash
# Build outside the explorer's clock; budget its complete raw output.
set -euo pipefail
export PATH="/usr/sbin:/sbin:$PATH"
for tool in cargo timeout python3 ntfsfix ntfsls ntfscat; do
    command -v "$tool" >/dev/null || { echo "$tool missing: install cargo-fuzz, coreutils, python3 and ntfs-3g" >&2; exit 1; }
done
cargo +nightly fuzz build operation_sequence
read -r lines bytes < <(awk '$1 == "suite" { print $2, $3 }' scripts/tier-budgets.txt)
bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored \
    op-sequence-explorer op-sequence-explorer "$lines" "$bytes" -- \
    python3 scripts/op-sequence-explorer.py "${1:-120}"
