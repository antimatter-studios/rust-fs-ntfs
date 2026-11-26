## Writing

| A 4Kn volume with 1 KiB FILE records is mounted writable; FILE and INDX records keep and build 512-byte update-sequence strides, and Windows' chkdsk, listing and hashes accept the files and directories created, written, renamed and removed | Supported | Unreleased (#1411) | #1396 | `write_4kn_windows.rs` graded by `grade-windows-4kn-write.ps1`; `small_records_4kn_windows.rs`; `mft_io` and `record_build` USA tests |
| A directory index leaf that a removal or a rename empties is refilled from its neighbour or merged into it, so no empty leaf stays among a directory's names | Supported | Unreleased (#1411) | #1396 | `write.rs` unit test; `write_4kn_windows.rs` graded by `grade-windows-4kn-write.ps1` |
| Renaming a name an interior index node holds moves it into a leaf without the child pointer it had there | Supported | Unreleased (#1411) | #1396 | `write.rs` unit tests |
