### Added

- A directory with an `$ATTRIBUTE_LIST` can outgrow its resident index
  (#816, part of #666). Root growth moves named streams and EAs out first;
  promotion places `$INDEX_ALLOCATION` and `$BITMAP` through the list,
  in the base record while they fit and in extension records otherwise.
  Independent directory enumeration and fsck cover resident and nonresident
  lists, extension placement, and subsequent INDX splits. The Windows
  directory-stream scenario now grows the index before chkdsk grades it.
