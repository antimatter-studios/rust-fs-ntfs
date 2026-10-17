## Interfaces

| `fs.ntfs truncate`: set a regular file's length in bytes through the driver's truncate, including resident promotion, cluster release and zero-filled growth; predictable refusals leave the image byte-identical (`--features cli`) | Supported; symbolic links and other reparse points are not followed, and compressed, sparse and encrypted data are refused | Unreleased | #655 | `tests/cli/test-truncate.sh`, `cli-truncate-win-verify-chkdsk` |
