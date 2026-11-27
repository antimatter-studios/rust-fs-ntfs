## Checking and recovery

| Durable rescue archive creation (`fs.ntfs SOURCE rescue ARCHIVE`, `fs_ntfs::rescue::scan_to_new_archive`): on Unix the new archive's parent directory is synced before the scan and any report; an unsyncable directory or a refused sector size leaves no archive | Experimental; Windows has no parent directory sync | Unreleased (#1417) | #1400 | `cli_rescue.rs` |
