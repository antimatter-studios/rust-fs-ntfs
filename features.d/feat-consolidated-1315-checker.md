## Checking and recovery

| `fsck.ntfs` / `fsck::check_io`: each base record's hard-link count against its `$FILE_NAME` attributes, DOS names not counted; a file with an `$ATTRIBUTE_LIST` is not compared | Supported | Unreleased (#1164) | #583 | `check_io_link_count.rs` |
