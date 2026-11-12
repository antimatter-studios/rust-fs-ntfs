### Added

- **A file's Linux owner, group and mode persist in WSL's `$LXUID`,
  `$LXGID` and `$LXMOD` attributes (#1195, part of #599).**
  `write::set_unix_metadata` and `write::read_unix_metadata`, and
  `Filesystem::set_unix_metadata` and `Filesystem::unix_metadata`, store
  and read them beside the native security descriptor, which is left
  untouched; Linux's ntfs3 reads what is written and is read back. A mode
  naming another kind of file, a device, FIFO, socket or symbolic link, or
  any mode on a reparse point is refused, with nothing written.
