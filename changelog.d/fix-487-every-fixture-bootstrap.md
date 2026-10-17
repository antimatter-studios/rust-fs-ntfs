### Fixed

- **No committed NTFS image carries formatter boot code (#487).** The two
  mkntfs-made fuzz volumes, their boot-sector seeds and the fragmented-INDX
  fixture keep their BPBs and signatures with the boot code zeroed, and the
  fuzz-corpus builder zeroes it before writing a seed. CI now checks every
  tracked NTFS image, every boot-sector seed and the data files the crate
  embeds.
