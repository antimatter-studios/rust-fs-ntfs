### Added

- **`fs.ntfs SOURCE rescue ARCHIVE` runs the rescue scan from the command
  line.** It creates a new rescue archive of the source, or with `--resume`
  carries on an interrupted one after checking the archived sectors still
  read the same. `--offset` rescues a partition inside a whole-disk source and
  `--sector-size` sets the retry unit. The JSON report gives the bytes
  archived, the unreadable bytes and spans, the split by metadata, allocated
  and free space, and whether the archive is complete. The exit status is 0
  for a complete archive, 5 when some spans are still unreadable, and 1 when
  the rescue failed or was refused; an existing archive is refused without
  `--resume`, with the source and the archive unchanged.
