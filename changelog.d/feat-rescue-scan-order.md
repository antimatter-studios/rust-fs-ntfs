### Changed

- **The rescue scan archives an NTFS source metadata first.** When the boot
  sector parses and `$Bitmap` reads in full, the scan reads the boot sector
  and its backup, `$MFT` and `$MFTMirr` first, then the clusters `$Bitmap`
  marks allocated, then the free ones, then the tail, so the bytes that
  matter most are copied before a failing medium gets worse. A source with no
  readable layout is still scanned linearly with every record unclassified.
