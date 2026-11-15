### Added

- **`rust-ntfs times` sets the four `$STANDARD_INFORMATION` times of a file
  or directory, and `rust-ntfs security-id` points one at a descriptor
  already in `$Secure` (#1065).** Each time is a decimal FILETIME, or `-` to
  leave it as it is; a security id that names no descriptor is refused.
