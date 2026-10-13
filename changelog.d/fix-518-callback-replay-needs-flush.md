### Fixed

- **A replay through callbacks that cannot flush is refused** (#518). A
  `$LogFile` replay is safe only if the replayed metadata reaches the disk
  before the log says it is clean. The read/write callback pair
  (`FsNtfsBlockdevCfg`) has no flush callback, so
  `fs_ntfs_mount_with_callbacks_replaying_log` and
  `fs_ntfs_fsck_with_callbacks` now refuse a log holding work, and the
  latter a dirty-flag repair, with nothing written, and name the route that
  can flush: an fs-core device from `fs_core_device_from_callbacks` with a
  flush callback. `BlockIo::can_sync` says which backends reach stable
  storage.
