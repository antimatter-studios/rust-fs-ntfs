## Checking and recovery

| Incomplete rescue export marks the volume dirty (`fs_ntfs::rescue::export_with`): an image with missing spans has `$Volume`'s dirty flag set, mirrored into `$MFTMirr`, and the report and manifest say whether it was; a missing `$Volume` record is named, not fabricated, and a complete export never touches the flag | Experimental; the Windows `fsutil dirty query` oracle is not yet written (#1003) | Unreleased (#1001) | #725 | `rescue_export.rs` |

## Making a filesystem

| Small-volume formatting places `$MFTMirr` past metadata when the midpoint overlaps it, retaining the overlap and backup-boot bounds | Supported | Unreleased (#1005) | #1290 | `a_small_volume_places_its_mirror_after_metadata_and_reads_back_independently`, `the_small_volume_layout_still_refuses_a_mirror_at_the_backup_boot_boundary`, `rescue_export.rs` |
