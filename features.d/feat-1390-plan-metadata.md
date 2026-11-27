## Checking and recovery

| `PrimitiveAPI` materialization plans from a salvage walk: serialization, check without reading, positioned reads, refusal past the source, unreadable ranges flagged, name and times (`NtfsAdapter::plan_metadata`) | Supported: bytes compared with the SHA-256 Windows reads, times with the independent `ntfs` reader; source overlaps are reported as findings, and a plan has no step for an unknown range | Unreleased (#1424) | #1390 | `tests/layers_windows.rs` |
