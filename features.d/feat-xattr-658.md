## Interfaces

| `fs.ntfs xattr PATH [NAME]`: native EA listing and raw value reads (`--features cli`) | Supported; Windows NtQueryEaFile supplies the names, sizes, NEED_EA and value-byte oracle, including non-resident EAs | Unreleased (#773) | #658 | `tests/cli/test-xattr.sh`, `tests/cli-oracle/test-xattr-windows.sh` |
