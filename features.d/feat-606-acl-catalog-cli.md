## Interfaces

| `fs.ntfs TARGET acl catalog [--id ID] [--jsonl]`: every `$Secure:$SDS` copy with its offsets, id, size, hashes, verdicts, mirror agreement and decoded descriptor, read-only | Supported: graded against the formatter's catalog and a Windows-authored catalog past one 256 KiB region | Unreleased (#606) | #606 | `tests/mkfs_bin_smoke.rs`, `tests/scripts/acl-catalog-windows-oracle.ps1` in `security-descriptor-oracle.yml` |
