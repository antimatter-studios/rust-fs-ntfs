## Checking and recovery

| `fsck.ntfs --dry-run` / `repair_plan::plan_fsck_io`: every write `fsck -y` would make, as units between flushes, each write's before- and after-image, none made | Supported; the planned writes are the ones the repair makes, and reach what Windows' own restart recovered on every interrupted capture | Unreleased (#709) | #589 | `windows_interrupted_fixture.rs` |
