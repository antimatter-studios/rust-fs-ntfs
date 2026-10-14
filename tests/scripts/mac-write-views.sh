#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
capture="$scratch/calls"
export capture

# The fake binary records one line per call: whether MSYS path conversion
# was off, then its arguments.
printf '%s\n' '#!/usr/bin/env bash' 'printf "%s|%s\n" "${MSYS_NO_PATHCONV:-}" "$*" >> "$capture"' > "$scratch/fake"
chmod +x "$scratch/fake"
bash "$repo/scripts/mac-write-views.sh" "$scratch/fake" volume.img /views 2
mapfile -t calls < "$capture"
want=(
    '1|symlink volume.img /views l0000 target.txt'
    '1|objid volume.img /views/l0000 9e3779b1000000000571057100000000'
    '1|symlink volume.img /views l0001 target.txt'
    '1|objid volume.img /views/l0001 3c6ef362000000010571057100000000'
)
[[ ${#calls[@]} -eq ${#want[@]} ]] || { echo "mac-write-views: ${#calls[@]} calls, not ${#want[@]}" >&2; exit 1; }
for i in "${!want[@]}"; do
    [[ ${calls[$i]} == "${want[$i]}" ]] || {
        echo "mac-write-views: call $i was '${calls[$i]}', not '${want[$i]}'" >&2
        exit 1
    }
done
echo 'mac-write-views: ok (native path conversion disabled, a symlink and an object id per link)'
