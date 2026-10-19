### Changed

- `fsck.ntfs --apply --in-place` holds its journal exclusively for the whole
  apply or resume, so a second resume of a journal another one holds is
  refused, naming the journal, before anything is written (#936).
