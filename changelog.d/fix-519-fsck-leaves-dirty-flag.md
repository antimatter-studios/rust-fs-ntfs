### Changed

- **fsck leaves the dirty flag set unless asked to clear it** (#519).
  `VOLUME_IS_DIRTY` asks for a check of the whole volume, which Windows'
  chkdsk makes at the next mount and this crate does not. Clearing it told
  Windows that check had been made. `fsck`, `fsck_io`, `fsck.ntfs -y` and
  the `fs_ntfs_fsck*` C functions still replay a `$LogFile` that holds
  work, but now leave the flag set; `FsckReport::dirty` says so, and
  `fsck.ntfs` exits 4 and names `--clear-dirty`. Clearing it is asked for
  with `fsck_with` / `fsck_io_with` and `FsckOptions { clear_dirty: true }`,
  `fsck.ntfs -y --clear-dirty` (`-d`, as ntfs-3g's `ntfsfix -d`), or
  `FS_NTFS_FSCK_CLEAR_DIRTY`. **Breaking:** the three `fs_ntfs_fsck*` C
  functions take a new last argument, `uint32_t flags`, and an unknown bit
  is EINVAL; `FsckReport` gains `dirty`.
