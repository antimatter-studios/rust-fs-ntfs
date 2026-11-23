### Added

- **NTFS through rust-fs-core's three API layers.** `fs_ntfs::layers::NtfsVolume` implements rust-fs-core 0.5's `VolumeAPI` and `OpenVolume` (the boot sector, its backup, geometry, health from the dirty flag and `$LogFile`, and a salvage open from the backup boot sector when the primary is gone), `PrimitiveAPI` with the on-disk structures (boot sectors, `$MFT`, `$MFTMirr`, `$LogFile`, `$Bitmap`, the root) and `NtfsAdapter` as its `SalvageAdapter`, and `FileAPI` by record and sequence number. The adapter's candidates are file records, live and deleted; a walk places each run at its file offset, with uninitialized bytes flagged, LZNT1 units as encoded extents and resident values in the record as stored; the tree's parent claims are every hard-link name. `NtfsAdapter::from_source` builds the adapter from whichever boot sector validates (#1385).

### Changed

- rust-fs-core is required as 0.5 with its `layers` feature.
- The driver's own traits are `NtfsVolumeAPI`, `NtfsPrimitiveAPI` and `NtfsFileAPI`, holding what is NTFS's own: records, streams, `$MFTMirr`, the allocation rebuilt from the records, and the writable surface. Their old names, `VolumeAPI`, `PrimitiveAPI` and `FileAPI`, remain for one release as deprecated traits that forward to them (#1385).
