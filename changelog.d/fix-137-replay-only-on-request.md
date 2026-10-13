### Fixed

- **A read-write mount replays a log holding work only when asked** (#137).
  Windows leaves a `$LogFile` holding work after a Fast Startup shutdown or
  a hibernation as well as after a crash, and in the first case it resumes
  from what it cached and does not see a replay's changes. Only
  `hiberfil.sys` tells them apart, and only on the volume Windows boots
  from. So `Filesystem::mount_rw`, `fs_ntfs_mount`, the writable
  `fs_ntfs_mount_with_callbacks`, `fs_ntfs_mount_rw_with_fs_core_device`,
  the path-based writers and `fs.ntfs` refuse such a log, with nothing
  written, and name `fsck.ntfs -y`. The replay is asked for with `fsck`,
  `Filesystem::mount_rw_replaying_log`, or the `_replaying_log` variant of
  each C ABI mount.
