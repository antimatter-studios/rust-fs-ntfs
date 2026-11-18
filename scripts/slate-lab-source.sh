#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 3 ] && [ "$1" = verify ]; then
    python3 - "$2" "$3" <<'PY'
import csv
import hashlib
import sys
from pathlib import Path

image = Path(sys.argv[2])
with open(sys.argv[1], newline="", encoding="utf-8") as handle:
    rows = [row for row in csv.DictReader(handle, delimiter="\t")
            if row["case"] == "slate-write-lab-source"]
if len(rows) != 1:
    sys.exit("the lab source must have exactly one Windows grade")
row = rows[0]
with image.open("rb") as handle:
    digest = hashlib.file_digest(handle, "sha256").hexdigest()
if (row["image"] != image.name or row["sha256"] != digest
        or row["windows_verdict"] != "clean" or row["windows_exit"] != "0"):
    sys.exit("the lab source is not the exact image Windows graded clean")
print("slate-lab-source: verified Windows' clean grade and source hash")
PY
    exit
fi

[ "$#" -eq 2 ] || { echo 'usage: slate-lab-source.sh SOURCE NEW_IMAGE' >&2; exit 2; }
source_image=$1
destination=$2
[ -f "$source_image" ] || { echo "missing source: $source_image" >&2; exit 1; }
[ ! -e "$destination" ] && [ ! -e "$destination.json" ] || {
    echo "lab source already exists: $destination" >&2
    exit 1
}
for tool in ntfs-3g.probe ntfscat ntfscp python3 sha256sum; do
    command -v "$tool" >/dev/null || { echo "missing $tool; install ntfs-3g and Python 3" >&2; exit 1; }
done
original_digest=$(sha256sum "$source_image" | cut -d' ' -f1)
ntfs-3g.probe -w "$source_image"
mkdir -p "$(dirname "$destination")"
work=$(mktemp -d "$(dirname "$destination")/.slate-lab-source.XXXXXX")
cleanup() {
    status=$?
    if [ "$(sha256sum "$source_image" | cut -d' ' -f1)" != "$original_digest" ]; then
        echo 'lab preparation changed its original source' >&2
        status=1
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
ntfscat "$source_image" '/$LogFile' >"$work/original-log"
python3 - "$work/original-log" "$work/empty-log" <<'PY'
import os
import sys

size = os.path.getsize(sys.argv[1])
if size < 8192 or size % 4096:
    sys.exit("lab source has no complete log pages")
with open(sys.argv[2], "xb") as output:
    for offset in range(0, size, 4096):
        output.write(b"\xff" * 4096)
PY
cp "$source_image" "$work/image"
ntfscp -f "$work/image" "$work/empty-log" '/$LogFile'
ntfscat "$work/image" '/$LogFile' >"$work/prepared-log"
cmp "$work/empty-log" "$work/prepared-log"
ntfs-3g.probe -w "$work/image"
prepared_digest=$(sha256sum "$work/image" | cut -d' ' -f1)
[ "$prepared_digest" != "$original_digest" ] || { echo 'lab preparation changed no bytes' >&2; exit 1; }
python3 - "$work/receipt.json" "$original_digest" "$prepared_digest" <<'PY'
import json
import sys

with open(sys.argv[1], "x") as output:
    json.dump({"schema": 1, "source_case": "cli-write-at-win-verify-chkdsk",
               "source_sha256": sys.argv[2], "lab_source_sha256": sys.argv[3],
               "preparation": "ntfscp replaces only the clean source copy's $LogFile with uninitialized pages"},
              output, indent=2)
    output.write("\n")
PY
[ "$(sha256sum "$source_image" | cut -d' ' -f1)" = "$original_digest" ] || {
    echo 'lab preparation changed its original source' >&2
    exit 1
}
ln "$work/image" "$destination"
ln "$work/receipt.json" "$destination.json"
echo "slate-lab-source: $original_digest -> $prepared_digest; original preserved"
