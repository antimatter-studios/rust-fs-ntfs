## Checking and recovery

| Rescue scan (`fs_ntfs::rescue::scan`): every readable byte of a failing source copied, read-only, into a new append-only archive of CRC-32-checked records; a failed bulk read retried sector by sector, and every unreadable span recorded with no payload (`docs/rescue-archive.md`) | Experimental; resume, export, allocated-first order and the CLI are not yet written | Unreleased | #595 | `rescue_scan.rs` |
