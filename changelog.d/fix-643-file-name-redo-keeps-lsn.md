### Fixed

- **A replayed `UpdateFileNameRoot` or `UpdateFileNameAllocation` leaves
  the block's LSN where Windows' restart leaves it** (#643). These
  operations refresh the copy of a file's `$FILE_NAME` that an index entry
  holds. `fsck` and every read-write mount that replays stamped the MFT
  record or index block with the redo's LSN. Windows' restart keeps the LSN
  of the block's newest other change. On every captured Windows volume in
  `test-disks/`, each replayed MFT record and index block now carries the
  LSN that Windows' restart left. The only exception is a block that undo
  rolls back.
