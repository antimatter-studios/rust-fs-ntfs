### Added

- **`fs.ntfs ln TARGET PATH` adds a hard link, and `ln -s` makes a symbolic
  link.** A hard link gives the file at TARGET a further name in any
  directory; a symbolic link's target is the text as given, relative, from
  the volume's root or a drive path, through the library's `link` and
  `create_symlink`. A directory is given no second name, a file no more
  than the 1024 Windows allows, and a name already at PATH, compared as
  Windows compares names, is refused. Every refusal is decided on a
  read-only open, with the edit rehearsed in full, so a refused command
  leaves the image byte-identical. Windows reads every file through each of
  its names, lists them with `fsutil hardlink list`, follows each symbolic
  link to its target and finds the volume clean with `chkdsk` (#646).

### Fixed

- Added hard-link names use the Windows-observed namespace in both their
  file record and directory index, instead of creating another combined
  DOS name for a short link. Existing names retain their namespace.

- **A hard link to a file already at 1024 links is refused, as Windows
  refuses it.** The library's `link` did not check the count, and added a
  name past it whenever the file's record had room (#646).
