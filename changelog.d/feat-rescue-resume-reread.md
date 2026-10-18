### Added

- **An interrupted rescue scan resumes in place.** `fs_ntfs::rescue::resume`
  cuts the torn trailing record from an archive and reads again only the
  ranges no data record holds, including the spans the earlier pass recorded
  as unreadable. A span that reads now gains a data record; the earlier
  unreadable record stays as history, and no whole record is rewritten.
