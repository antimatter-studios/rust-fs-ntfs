## Interfaces

| `fs.ntfs export PATH DEST [--replace]` (`--features cli`) | Supported for uncompressed regular files; bounded chunks, SHA-256, stdout and atomic publication; reparse points and transformed streams refused | Unreleased (#834) | #832 | `tests/cli-oracle/test-export-windows.sh` |
| `fs.ntfs export --recursive` | Unsupported; blocked on the shared core tree/metadata release | — | #833 | Pending the released tree source traits |

## Writing

| Security references on file and directory creation | Validates and reuses nonzero parent catalog IDs; native ACL inheritance remains unsupported | Unreleased (#834) | #586, #590 | `mkfs_roundtrip.rs`, Slate writer oracle |
