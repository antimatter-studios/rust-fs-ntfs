## Checking and recovery

| `PrimitiveAPI` record evidence: the record's own number, every resident `$FILE_NAME` (parent reference, namespace, name, flags, times, sizes), `$STANDARD_INFORMATION`, whether an `$ATTRIBUTE_LIST` is present, and the `$Volume` record's `$VOLUME_INFORMATION` and label, from the bytes of one inspected record | Supported: compared with the independent `ntfs` reader | Unreleased (#1378) | #1378 | `tests/three_api.rs` |
| `mft_io::parse_boot_sector`: validated geometry from boot-sector bytes in memory, without a device | Supported: compared with the geometry of the opened volume | Unreleased (#1378) | #1378 | `tests/three_api.rs` |
