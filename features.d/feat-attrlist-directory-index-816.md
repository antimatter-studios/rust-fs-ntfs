## Writing

| A directory with an `$ATTRIBUTE_LIST` grows its resident index by moving streams and EAs out first, and lists its new `$INDEX_ALLOCATION` and `$BITMAP` when it spills into INDX blocks | Supported | Unreleased (#955) | #816 | `write::tests::listed_directory_index_promotion_keeps_every_attribute_listed`, `write::tests::listed_directory_root_growth_moves_streams_before_spilling`; Windows `mac-format-directory-streams-win-chkdsk` |
