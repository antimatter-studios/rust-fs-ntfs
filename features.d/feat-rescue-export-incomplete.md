## Checking and recovery

| Incomplete rescue export (`fs_ntfs::rescue::export_with`, `allow_incomplete`): an archive lacking some source bytes exports only on request, its missing spans written as zeros and listed by byte offset and length in an error manifest beside the image, and the report says the image is incomplete | Experimental; the dirty flag, the manifest's file attribution and the Windows oracle are not yet written (#1001, #1002, #1003) | Unreleased (#1000) | #725 | `rescue_export.rs` |
