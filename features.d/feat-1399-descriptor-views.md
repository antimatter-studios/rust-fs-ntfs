## Interfaces

| C ABI `fs_ntfs_sd_parse` / `fs_ntfs_sd_ace`: revision, control, owner, group, absent/null/empty/non-empty DACL and SACL, ACEs in stored order (types 0, 1, 2, 5, 6, 7, 9, 10, 11 and 13 decoded, others opaque) with raw callback data, and located malformed-input errors | Supported | Unreleased (#1416) | #1399 | `tests/capi_security_descriptor.rs`, `tests/security_descriptor_windows_oracle.rs` |
