#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/tests/scripts"
printf '# No exceptions\n' > "$scratch/tests/scripts/whole-read-allowlist.txt"
for call in 'std::fs::read(&image)' 'fs::read(&disk)' $'std::fs::read(\n &unknown\n)' \
    'use std::fs::read; read(&image)' 'use std::fs::read as load; load(&image)' \
    'use std::fs::{self, read as load}; load(&image)' 'use std::fs as diskfs; diskfs::read(&image)' \
    '::std::fs::read(&image)' 'use std::{fs::read as load}; load(&image)' \
    'use std::{fs as diskfs}; diskfs::read(&image)' 'use std :: fs :: read as load; load(&image)' \
    'std::fs::read::<_>(&image)' 'use std::fs::read as load; load::<_>(&image)'; do
    printf 'fn example() { %s; }\n' "$call" > "$scratch/tests/example.rs"
    if python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$scratch" > "$scratch/result" 2>&1; then
        echo "whole-image guard missed: $call" >&2
        exit 1
    fi
    grep -q 'tests/example.rs:1:' "$scratch/result"
done
printf 'fn example() { std::fs::read(&tiny_seed); }\n' > "$scratch/tests/example.rs"
printf 'tests/example.rs:1:std::fs::read(&tiny_seed) # A fixed eight-byte regression seed.\n' \
    > "$scratch/tests/scripts/whole-read-allowlist.txt"
python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$scratch"
printf '\nfn example() { std::fs::read(&tiny_seed); }\n' > "$scratch/tests/example.rs"
if python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$scratch" > "$scratch/result" 2>&1; then
    echo "whole-image guard allowed a different call site to reuse an exception" >&2
    exit 1
fi
grep -q 'tests/example.rs:2:' "$scratch/result"
printf '# No exceptions\n' > "$scratch/tests/scripts/whole-read-allowlist.txt"
printf 'const EXAMPLE: &str = "std::fs::read(&image)"; // fs::read(&disk)\n' > "$scratch/tests/example.rs"
python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$scratch"
mkdir -p "$scratch/src"
printf 'fn example() { std::fs::read(&image); }\n' > "$scratch/src/mft_inventory.rs"
if python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$scratch" > "$scratch/result" 2>&1; then
    echo "whole-image guard missed a source-embedded unit-test read" >&2
    exit 1
fi
grep -q 'src/mft_inventory.rs:1:' "$scratch/result"
python3 - "$ROOT" <<'PY'
import runpy
import sys

calls = runpy.run_path(sys.argv[1] + "/scripts/check-whole-image-reads.py")["calls"]
for source in [
    """let quote = '"'; fs::read(&image); let end = "done";""",
    """fn example<'a>(value: &'a str) { fs::read(&image); }""",
    """/* outer /* inner */ "quoted */ fs::read(&image); let end = "done";""",
    '''let doc = r#"fs::read(&not_a_call)"#; fs::read(&image);''',
]:
    assert len(list(calls(source))) == 1, source
assert not list(calls("/* outer /* inner */ fs::read(&not_a_call) */"))
PY
python3 "$ROOT/scripts/check-whole-image-reads.py" --root "$ROOT"
