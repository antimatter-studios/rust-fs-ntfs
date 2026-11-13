## Interfaces

| `fs.ntfs` `write`, `write --at` and `truncate` on a file whose unnamed `$DATA` is held only in an extension record, through its `$ATTRIBUTE_LIST` (`--features cli`) | Supported | Unreleased (#1328) | #1328 | `cli_listed_data_extension.rs`, matrix `cli-attrlist-split-extents-win-verify-chkdsk` |

## Writing

| A write at an offset, a rewrite or a truncate of an unnamed `$DATA` reached through an `$ATTRIBUTE_LIST` reuses the clusters it has, and asks the volume only for those it grows by | Supported | Unreleased (#1328) | #1347 | `attribute_list_split_extents_resize.rs`, matrix `cli-attrlist-split-extents-win-verify-chkdsk` |
