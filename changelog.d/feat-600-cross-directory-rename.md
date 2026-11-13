### Added

- **A file moves into another directory (part of #600).** `write::move_to`
  and `fs.ntfs mv` move a regular file into another directory, under its
  own name or a new one, into and out of directories whose index lives in
  INDX blocks, and one name of a hard-linked file while its other names
  stay. The new name is added before the old one goes, so a move cut short
  leaves the file with both names and a link count of two, never with no
  name. A directory, and a file with a separate DOS 8.3 name, are still
  refused before anything is written.
