### Added

- **`mkfs.ntfs --dry-run` reports the format plan.** The JSON report carries a `plan` object: bytes per sector, sectors per cluster, cluster, record and index block sizes, total sectors and clusters, where `$MFT`, `$MFTMirr` and `$LogFile` go, and the label and serial. A format reports the plan it applied, the same one a dry run with the same flags reports. Library users get it from `fs_ntfs::mkfs::plan_format`, and `format_with_plan` writes exactly the plan given.

### Changed

- **A dry run refuses what formatting would refuse.** `mkfs.ntfs --dry-run` (and `rust-ntfs format -n`) used to accept a geometry the formatter then refused, such as a 3000-byte cluster or a volume too small for the layout; it now exits 1 with the formatter's reason and writes nothing.
- **`format_filesystem` refuses a label longer than 32 UTF-16 code units**, which `$VOLUME_NAME` cannot hold, instead of writing it. The command line already refused one.
- **4K and larger formats initialize `$Quota`'s default policy control**, which Slate requires and Windows may not create on first mount. Smaller cluster sizes retain the initialized quota record required by `chkdsk`.
