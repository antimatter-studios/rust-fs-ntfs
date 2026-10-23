### Fixed

- **A replay leaves `$LogFile` as a clean Windows dismount does (#762).**
  It filled the log with `0xFF`, and Windows, mounting the volume
  afterwards, could start its own log at LSNs at or below those the replay
  left in MFT records and index blocks, which its next restart would then
  skip. The replay now leaves two clean restart pages and a checkpoint with
  empty tables, naming a current LSN one wrap past the last it replayed.
  `fsck.ntfs` reports such a log as `clean` rather than `empty`.
