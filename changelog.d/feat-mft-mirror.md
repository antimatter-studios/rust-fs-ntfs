### Added

- **`$MFTMirr` is a salvage source.** `BootParams` gains `mft_mirror_lcn` (boot sector offset 0x38), `mft_mirror_offset()` and `mft_mirror_records()`. `PrimitiveAPI::read_mirror_record` reads one of `$MFT`'s first records from the mirror with the same fixup checks and damage reports as a `$MFT` read, and `read_mft_map_from_mirror` builds the map of where file records are from the mirror's copy of record 0, saying so in its limitations, for a volume whose own record 0 is damaged (#1386).
