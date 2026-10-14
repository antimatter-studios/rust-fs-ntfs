#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/sbin:/sbin:$PATH"
for tool in ntfsfix ntfscat; do
    command -v "$tool" >/dev/null || { echo "$tool missing: install ntfs-3g" >&2; exit 1; }
done
export OPSEQ_REFERENCE_TOOLS=1
bash ../rust-fs-core/scripts/tier.sh --refuse-skips mkfs -- \
    cargo test --release --locked --test op_sequence_readback -- --test-threads=1
n=$(grep -aoE 'test result: ok\. [0-9]+ passed' tmp/logs/mkfs.log | awk '{s+=$4} END{print s+0}')
test "$n" -eq 4 || { echo "op-sequence oracle: $n executed, expected exactly 4" >&2; exit 1; }
