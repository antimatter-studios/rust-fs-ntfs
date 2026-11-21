## Reading

| Volumes on 4096-byte sectors with 1024-byte file records, update sequence arrays with 512-byte strides inside the sector | Supported: Windows formats the disk and writes the files; geometry compared with fsutil, every file with `Get-FileHash`, chkdsk clean | Unreleased (#1384) | #1384 | `tests/small_records_4kn_windows.rs` |

## Writing

| Writing a volume whose file records are smaller than its sectors | Refused by name: the writers build update sequence arrays with the sector stride | Unreleased (#1384) | #1384 | `tests/small_records_4kn_windows.rs` |
