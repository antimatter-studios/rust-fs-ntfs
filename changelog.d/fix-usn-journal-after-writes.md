### Fixed

- **A write no longer leaves a change journal that does not record it
  (#479).** On a volume with an active change journal (`$Extend\$UsnJrnl`),
  this crate's writes were never appended to `$UsnJrnl:$J`, and the journal
  kept its id and its next USN, so a reader carrying on from its last USN
  (Windows Search, backup, DFS-R, antivirus) missed them with no way to
  tell. Before its first write the crate now sets `$Volume`'s `0x0008`
  flag, the mark of a writer that does not keep the journal, and Windows
  answers it on its next mount by giving the journal a new id, which tells
  every reader to scan the volume again. The journal stays active, at the
  size it was created with. A journal that cannot be retired refuses the
  write. The writes themselves are not yet recorded in the journal.
