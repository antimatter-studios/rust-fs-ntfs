### Fixed

- **Windows lists a written symlink as a link.** Writing or removing a
  reparse point now sets or clears the reparse bit and tag in every
  directory entry of the file, as Windows does for its own symlinks.
  Before this, Windows listed a symlink this crate wrote as a plain file,
  and opening that file followed the link (#1357).
