#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
capture="$scratch/calls"
export capture

# One line per call: the path-conversion switch, then the arguments.
printf '%s\n' '#!/usr/bin/env bash' 'printf "%s|" "${MSYS_NO_PATHCONV:-}" "$@" >> "$capture"; echo >> "$capture"' > "$scratch/fake"
chmod +x "$scratch/fake"
bash "$repo/scripts/mac-write-streams.sh" "$scratch/fake" volume.img /f 3 20 2
mapfile -t calls < "$capture"
want=(
    "1|stream|volume.img|/f|s1|--content|stream 1 stream 1 st|"
    "1|stream|volume.img|/f|s2|--content|stream 2 stream 2 st|"
    "1|stream|volume.img|/f|s3|--content|stream 3 stream 3 st|"
    "1|stream|volume.img|/f|s2|--delete|"
)
[[ ${#calls[@]} -eq ${#want[@]} ]] || { echo "mac-write-streams: ${#calls[@]} calls, want ${#want[@]}" >&2; exit 1; }
for i in "${!want[@]}"; do
    [[ ${calls[$i]} == "${want[$i]}" ]] || {
        echo "mac-write-streams: call $i was '${calls[$i]}', want '${want[$i]}'" >&2
        exit 1
    }
done

if bash "$repo/scripts/mac-write-streams.sh" "$scratch/fake" volume.img /f 3 x 0 2>/dev/null; then
    echo "mac-write-streams: a size that is not a number was accepted" >&2
    exit 1
fi
echo 'mac-write-streams: ok (contents, deletions and native path conversion as the matrix needs)'
