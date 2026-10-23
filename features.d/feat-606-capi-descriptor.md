## Interfaces

| C ABI `fs_ntfs_read_security_descriptor`: a file's stored self-relative security descriptor, its own `$SECURITY_DESCRIPTOR` or the `$SDS` copy its security id names (a verified primary, else its verified mirror), with a size query and nothing written into a short buffer | Supported | Unreleased (#606) | #606 | `src/lib.rs` (`capi_read_tests`), `include/fs_ntfs.h` |
