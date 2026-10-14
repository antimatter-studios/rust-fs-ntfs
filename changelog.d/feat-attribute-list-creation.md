### Added

- **A named stream that its file's base record cannot hold goes to an
  extension record named in an `$ATTRIBUTE_LIST`, as Windows does (#480).**
  A third or fourth stream on a full 1 KiB record was refused with "no room
  for new attribute". The file is now given a list naming every attribute,
  its named streams move to extension records, and the list goes to
  clusters once the base record cannot hold it. The named streams of a
  file that already has a list, whichever implementation made it, are
  written, rewritten and deleted through it, and an extension record left
  empty is freed. Any other attribute added to or removed from
  such a file is still refused, and so is a stream that a directory's
  record cannot hold.
- **`rust-ntfs stream` writes or deletes one of a file's named streams.**
