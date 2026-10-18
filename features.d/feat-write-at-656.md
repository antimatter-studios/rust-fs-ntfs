## Interfaces

| `fs.ntfs write --at`: patch an existing regular file without truncating, including resident promotion and zero-filled growth (`--features cli`) | Supported; reparse points, compressed, sparse and encrypted streams are refused before a writable open | Unreleased (#753) | #656 | `tests/cli/test-write-at.sh`, `cli-write-at-win-verify-chkdsk` |
