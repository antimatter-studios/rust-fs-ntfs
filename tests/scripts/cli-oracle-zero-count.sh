#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sandbox="$(mktemp -d)"
trap 'rm -r "$sandbox"' EXIT
mkdir -p "$sandbox/bin" "$sandbox/model"
export REAL_HEAD="$(command -v head)"
export HEAD_ZERO_LOG="$sandbox/zero-counts"

cat >"$sandbox/bin/head" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = -c ] && [ "$2" -eq 0 ]; then
    printf '%s\n' "$*" >>"$HEAD_ZERO_LOG"
    echo 'head: illegal byte count -- 0' >&2
    exit 1
fi
exec "$REAL_HEAD" "$@"
STUB

cat >"$sandbox/bin/ntfs" <<'STUB'
#!/usr/bin/env python3
import json
import pathlib
import sys

tool, image, *arguments = sys.argv[1:]
if tool == "fsck":
    sys.exit(0)
if arguments == ["get", "dirty", "--text"]:
    print("false")
    sys.exit(0)
if tool != "fs" or arguments[0] != "write":
    sys.exit("unexpected CLI call")
directory = pathlib.Path(image + ".data")
directory.mkdir(exist_ok=True)
positioned = arguments[1] == "--at"
offset = int(arguments[2]) if positioned else 0
name = arguments[3] if positioned else arguments[1]
path = directory / name.lstrip("/")
patch = sys.stdin.buffer.read()
contents = bytearray(path.read_bytes()) if positioned else bytearray()
if patch:
    contents.extend(bytes(max(0, offset + len(patch) - len(contents))))
    contents[offset:offset + len(patch)] = patch
path.write_bytes(contents)
print(json.dumps({"size": len(contents), "at": offset, "bytes": len(patch)}))
STUB

chmod +x "$sandbox/bin/head" "$sandbox/bin/ntfs"
export PATH="$sandbox/bin:$PATH"
export RUST_FS_NTFS="$sandbox/bin/ntfs"
fails=0
fail() { echo "FAIL  $*" >&2; fails=$((fails + 1)); }

if bash "$ROOT/scripts/cli-oracle.sh" write-at "$sandbox/image" >"$sandbox/write-at.log" 2>&1; then
    [ "$(wc -l <"$sandbox/image.manifest" | tr -d ' ')" -eq 8 ] || fail 'write-at did not finish every case'
    [ "$(wc -c <"$sandbox/image.data/empty" | tr -d ' ')" -eq 3 ] || fail 'empty input was not patched'
    [ "$(wc -c <"$sandbox/image.data/null" | tr -d ' ')" -eq 12289 ] || fail 'empty patch changed the size'
else
    cat "$sandbox/write-at.log" >&2
    fail 'write-at passed a zero byte count to head'
fi

: >"$HEAD_ZERO_LOG"
if ! (
    set -euo pipefail
    work="$sandbox/model"
    image="$sandbox/resize-image"
    run() { out="{\"size\": $5}"; status=0; }
    has() { grep -q -- "$1" <<<"$out"; }
    die() { echo "$*" >&2; exit 1; }
    same() { cmp "$work/model" "$work/expected" || exit 1; }
    eval "$(sed -n '/^        resized() {$/,/^        }$/p' "$ROOT/scripts/cli-oracle.sh")"
    printf 'contents' >"$work/model"
    printf 'con' >"$work/expected"
    resized /file 3
    printf 'con\000\000' >"$work/expected"
    resized /file 5
    resized /file 5
    : >"$work/expected"
    resized /file 0
    resized /file 0
) >"$sandbox/resized.log" 2>&1; then
    cat "$sandbox/resized.log" >&2
    fail 'resized passed a zero byte count to head or made incorrect bytes'
fi

if [ -s "$HEAD_ZERO_LOG" ]; then
    cat "$sandbox/resized.log" >&2
    fail 'resized passed a zero byte count to head'
fi
[ "$fails" -eq 0 ] || exit 1
echo 'cli-oracle-zero-count: all checks passed'
