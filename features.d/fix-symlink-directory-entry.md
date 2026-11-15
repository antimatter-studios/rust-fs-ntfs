## Writing

| Reparse points and symlinks: write and remove | Supported; every directory entry of the file carries the reparse bit and tag, as Windows writes them, so Windows lists a written symlink as a link | 0.1.0; directory entries Unreleased (#1334) | | `write_reparse.rs`, `readlink_windows_oracle.rs::a_written_symlink_carries_the_directory_fields_windows_writes` |
