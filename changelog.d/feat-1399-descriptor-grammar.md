### Added

- A malformed security descriptor names the damaged part: `security_descriptor::DecodeError` carries `component` and `ace_index`, and `fs_ntfs_sd_error_t` carries `component` (`FS_NTFS_SD_IN_*`) and `ace_index` (#1399).
