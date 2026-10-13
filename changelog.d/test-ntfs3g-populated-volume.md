### Added

- **A volume ntfs-3g populated, read back and written (#485).**
  `test-disks/build-ntfs-feature-images.sh populated` fills a 256 MiB
  volume at 512-byte and 4 KiB clusters the way a user's would be: a
  directory of 600 names, a compressed directory, one file with sixty long
  names, one with forty-one named streams, a sparse file, two fragmented
  files and names across UTF-16. It writes a manifest of what the mount
  reported beside each. `tests/ntfs3g_populated.rs` reads every directory,
  file and stream back against it and runs `check_io`. It then writes to a
  copy, and has ntfs-3g's `ntfscat` and `ntfs-3g.probe` read back what was
  written.
