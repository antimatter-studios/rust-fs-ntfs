## Interfaces

| Windows 4096-byte-sector format capture (test support) | Experimental: hosted Windows formats a disk that reports 4096-byte logical sectors and records `Get-Disk`'s sector sizes, native fsutil fields, raw boot sector fields, which backup boot locations hold a copy, and the update sequence offset and count of `$MFT` record 0 and of the root directory's first index block; the formatter does not yet use them (#897) | Unreleased (#900) | #896, #701, #588 | `tests/scripts/windows-format-geometry.ps1` (48 contracts), `.github/workflows/format-geometry.yml` |
