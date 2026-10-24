## Interfaces

| `fs.ntfs import SRC PATH [--verify]` (`--features cli`) | Supported for one regular file or stdin; bounded chunks, SHA-256, staged publication by same-directory rename; `--replace` exits 3 | Unreleased | #842, #1079 | `tests/cli-oracle/test-import-ntfs3g.sh`; Windows: `cli-import-win-verify-chkdsk` (chkdsk, in-place and copy-out SHA-256) |
| `fs.ntfs import --recursive` | Unsupported; blocked on the shared core tree source release | — | #843 | Pending the released tree source traits |
