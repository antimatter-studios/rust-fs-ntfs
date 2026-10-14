### Added

- **A `$LogFile` holding `UpdateRecordDataAllocation` redo is replayed**
  (#137). Once a volume's file owners outnumber what `$Quota`'s `$Q` index
  root holds, their entries move into index blocks, and NTFS logs each
  charge to them as this operation. `fsck` and every read-write mount that
  replays refused any log holding one. They now overwrite the entry's data
  and leave the block's LSN where it was, as Windows does. A volume Windows
  left mid-write with 48 owners, new in `test-disks/`, replays to what
  Windows' own restart produced from it.
- **The `$LogFile` capture's `mix: views` gives every file one of 48
  owners**, so `$Q` outgrows its root and captures hold
  `UpdateRecordDataAllocation` (#137).
