## Checking and recovery

| `$MFTMirr` location (`BootParams::mft_mirror_lcn`) and records read from it (`PrimitiveAPI::read_mirror_record`, `read_mft_map_from_mirror`) when `$MFT`'s first records are damaged | Supported: on a Windows-written volume with `$MFT` records 0 to 3 destroyed, the mirror returns the records Windows wrote and a file is read back to the SHA-256 Windows reported; a torn, zeroed or unreachable mirror is reported as damage | Unreleased (#1386) | #1386 | `tests/mft_mirror_windows.rs` |
