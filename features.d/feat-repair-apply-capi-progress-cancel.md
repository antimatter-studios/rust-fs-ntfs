## Checking and recovery

| C ABI in-place repair by a saved plan (`fs_ntfs_repair_apply_in_place`, `fs_ntfs_repair_resume_in_place`) with progress and cancellation callbacks | Supported on Unix with `plan-file`; cancelled only at a unit boundary, returning `FS_NTFS_REPAIR_CANCELLED` with a journal that resumes through the C ABI to what Windows' own restart recovered | Unreleased (#957) | #717 | `windows_interrupted_fixture.rs` |
