## Writing

remove: A directory needing more than 64 index blocks (a non-resident `$Bitmap:$I30`)

| Directories past `$INDEX_ROOT`: entries added and removed, index blocks split, the index gaining levels | Supported, while the index bitmap fits the directory's MFT record | Unreleased (#1415) | #1398 | `large_directory.rs`, `index_bitmap_growth.rs`, `indx_insert_bounds.rs`, `indx_insert_interior.rs`; Windows matrix `win-format-win-write-many-mac-insert-index-allocation-win-chkdsk` |
| A directory whose index bitmap outgrows its MFT record, or is already non-resident | Refused when an insert needs a new index block | Unreleased (#1415) | #1398 | |
