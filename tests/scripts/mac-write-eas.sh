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
bash "$repo/scripts/mac-write-eas.sh" "$scratch/fake" volume.img /f 3 12 2
mapfile -t calls < "$capture"
want=(
    "1|ea|volume.img|/f|USER.E1|--content|ea 1 ea 1 ea|"
    "1|ea|volume.img|/f|USER.E2|--content|ea 2 ea 2 ea|"
    "1|ea|volume.img|/f|USER.E3|--content|ea 3 ea 3 ea|"
    "1|ea|volume.img|/f|USER.E2|--delete|"
)
[[ ${#calls[@]} -eq ${#want[@]} ]] || { echo "mac-write-eas: ${#calls[@]} calls, want ${#want[@]}" >&2; exit 1; }
for i in "${!want[@]}"; do
    [[ ${calls[$i]} == "${want[$i]}" ]] || {
        echo "mac-write-eas: call $i was '${calls[$i]}', want '${want[$i]}'" >&2
        exit 1
    }
done

if bash "$repo/scripts/mac-write-eas.sh" "$scratch/fake" volume.img /f 3 x 0 2>/dev/null; then
    echo "mac-write-eas: a size that is not a number was accepted" >&2
    exit 1
fi
echo 'mac-write-eas: ok (contents, removals and native path conversion as the matrix needs)'
