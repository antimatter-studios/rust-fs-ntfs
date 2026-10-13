### Fixed

- **`$MFTMirr` on large clusters is what Windows writes (#481).** When a
  cluster holds more than four MFT records, the mirror now declares and
  holds a whole cluster of them, as Windows' formatter does: 64 records at
  64 KiB clusters, where it held four. The records it covers past the
  sixteen system ones are formatted as free records rather than left as
  zeros, every write to a record the mirror holds updates the mirror, and
  `fsck.ntfs` compares as many records as the mirror declares. ntfs-3g no
  longer warns about zero records reading such a volume.
