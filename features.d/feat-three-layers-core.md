## Interfaces

| rust-fs-core three layers (`fs_ntfs::layers::NtfsVolume`: `VolumeAPI`, `OpenVolume`, `PrimitiveAPI` with `NtfsAdapter` as its `SalvageAdapter`, `FileAPI`) | Supported: rust-fs-core's conformance set and checks pass on this crate's own volumes and on two Windows-built volumes, whose files read through `FileAPI`, and rebuilt from their salvage walks, match the SHA-256 Windows read | Unreleased (#1385) | #1385 | `tests/layers_windows.rs`, `tests/layers_conformance.rs` |
| `VolumeAPI`, `PrimitiveAPI`, `FileAPI` (the driver's own trait names) | Deprecated: renamed `NtfsVolumeAPI`, `NtfsPrimitiveAPI`, `NtfsFileAPI`; the old names forward for one release | Unreleased (#1385) | #1385 | `tests/deprecated_names.rs` |
