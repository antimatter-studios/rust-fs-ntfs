### Added

- **`fs_ntfs::rescue::scan_to_new_archive`** scans any `RescueSource` into a
  new archive whose directory entry is durable before the scan starts.

### Fixed

- **`fs.ntfs SOURCE rescue ARCHIVE` syncs a new archive's parent directory
  before it reports success.** It reported an archive a power cut could still
  lose. A rescue whose directory cannot be synced now fails and leaves no
  archive, and so does `fs_ntfs::rescue::scan_to_path`, which left an empty
  file behind.
- **A rescue refused for its sector size creates no archive.** The command
  line left an empty file that blocked the corrected command.
