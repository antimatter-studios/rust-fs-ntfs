## Checking and recovery

| Incomplete rescue export marks the volume dirty (`fs_ntfs::rescue::export_with`): an image with missing spans has `$Volume`'s dirty flag set, mirrored into `$MFTMirr`, and the report and manifest say whether it was; a missing `$Volume` record is named, not fabricated, and a complete export never touches the flag | Experimental; the Windows `fsutil dirty query` oracle is not yet written (#1003) | Unreleased (#1001) | #725 | `rescue_export.rs` |
