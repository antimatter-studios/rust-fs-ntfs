## Interfaces

| Windows large-cluster metadata layout capture (test support) | Experimental: for 64 KiB to 2 MiB clusters without large records, hosted Windows records the starting LCN and allocated clusters of `$MFTMirr`, `$Secure:$SDS` and the root `$INDEX_ALLOCATION` through `FSCTL_GET_RETRIEVAL_POINTERS`; the formatter does not yet use them (#893) | Unreleased (#892) | #892, #700, #588 | `tests/scripts/windows-format-geometry.ps1` (33 contracts), `.github/workflows/format-geometry.yml` |
