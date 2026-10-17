#!/usr/bin/env bash
# The repository source policy accepts oracle commands and rejects source links
# and unverifiable historical claims. The same check gates the real tree.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/src" "$scratch/docs"
printf 'Project\n' > "$scratch/README.md"
printf 'Agent instructions\n' > "$scratch/AGENTS.md"
passed=0
failed=0

check() {
    local name="$1" want="$2" text="$3" result
    printf '%s\n' "$text" > "$scratch/src/example.rs"
    python3 "$ROOT/scripts/check-source-policy.py" --root "$scratch" > "$scratch/result" 2>&1
    result=$?
    if { [ "$want" = pass ] && [ "$result" -eq 0 ]; } ||
       { [ "$want" = fail ] && [ "$result" -ne 0 ]; }; then
        passed=$((passed + 1))
    else
        printf 'FAIL %s: expected %s, exit %s\n' "$name" "$want" "$result"
        cat "$scratch/result"
        failed=$((failed + 1))
    fi
}

check microsoft-docs pass '//! https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header'
check permissive-source pass '//! https://github.com/ColinFinck/ntfs/blob/main/src/lib.rs'
check oracle-tool-names pass '//! ntfs-3g and ntfsfix are subprocess test oracles.'
check future-rule pass '//! Do not inspect GPL, LGPL or AGPL implementation source.'
check header-source fail '//! https://github.com/tuxera/ntfs-3g/blob/example/include/ntfs-3g/layout.h'
check raw-header-source fail '//! https://raw.githubusercontent.com/tuxera/ntfs-3g/example/include/ntfs-3g/layout.h'
check kernel-header-source fail '//! https://github.com/torvalds/linux/blob/example/fs/ntfs3/ntfs.h'
check encoded-source-path fail '//! https://github.com/torvalds/linux/blob/example/fs%2Fntfs3%2Fntfs.h'
check kernel-browser fail '//! https://elixir.bootlin.com/linux/example/source/fs/ntfs3/ntfs.h'
check header-without-url fail '//! The layout is corroborated by ntfs-3g layout.h.'
check kernel-header-without-url fail '//! The layout follows Linux ntfs3 ntfs.h.'
check historical-claim fail '//! References (no GPL code consulted): a layout.'
check wrapped-claim fail $'//! No GPL implementation\n//! was consulted.'
check tool-name-ban fail 'No mention of ntfs-3g anywhere in documentation.'
check fscc-reparse pass '//! REPARSE_DATA_BUFFER (MS-FSCC §2.1.2): u32 ReparseTag.'
check fscc-attributes pass '/// FILE_ATTRIBUTE_SPARSE_FILE (MS-FSCC 2.6).'
check fscc-information-class fail '/// Full $STANDARD_INFORMATION value (MS-FSCC §2.4.2).'
check fscc-fsctl fail '/// Attribute offsets are 8-aligned (MS-FSCC §2.3).'
check windows-internals fail '// Offsets per Windows Internals 7th ed.'

python3 "$ROOT/scripts/check-source-policy.py"
result=$?
if [ "$result" -eq 0 ]; then
    passed=$((passed + 1))
else
    printf 'FAIL repository-source-policy: exit %s\n' "$result"
    failed=$((failed + 1))
fi
printf 'source policy: %d passed, %d failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
