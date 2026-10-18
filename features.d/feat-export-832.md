## Interfaces

| `fs.ntfs export PATH DEST [--replace]` (`--features cli`) | Supported for regular files, LZNT1-compressed ones one compression unit at a time; bounded chunks, SHA-256, stdout and atomic publication; reparse points and encrypted streams refused | Unreleased (#834) | #832, #835 | `tests/cli-oracle/test-export-windows.sh` |
| `fs.ntfs export --recursive` | Unsupported; blocked on the shared core tree/metadata release | — | #833 | Pending the released tree source traits |
