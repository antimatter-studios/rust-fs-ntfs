### Added

- **`fs_ntfs::quota` reads `$Extend\$Quota`'s two indexes (#1085).** `tables_io` returns every `$O` entry (owner SID to owner id) and every `$Q` record (flags, bytes used, change time, limits and SID), `QuotaRecord::parse` and `sid_string` decode them, and `charge_io` reads the owner id and charge in one file's `$STANDARD_INFORMATION`. `examples/quota_dump.rs` prints all of it.
