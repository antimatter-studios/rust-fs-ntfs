### Added

- **A read-only Python ctypes client reads volume information through the
  C ABI** (#604). `python/fs_ntfs_volume_info.py` loads a shared library from
  an explicit absolute path, declares the argument and return types of
  `fs_ntfs_mount`, `fs_ntfs_umount`, `fs_ntfs_get_volume_info_v2`,
  `fs_ntfs_last_errno` and `fs_ntfs_last_error`, and mirrors
  `fs_ntfs_volume_info_v2_t`. A volume is a context manager whose `close()`
  is idempotent; use after close, an embedded NUL in the image path and C
  ABI failures each raise a named error. `cargo rustc --release --lib
  --crate-type cdylib` builds the shared library without changing the
  crate's declared staticlib and rlib outputs. CI compiles a C probe that
  reports the compiler's own `offsetof` and `sizeof` and the values a C
  caller reads, and requires the Python client to agree with both on a
  Windows-recovered fixture.
- **The Python client lists directories read-only through the C ABI**
  (#604). `Volume.open_dir` returns a context-managed iterator over
  `fs_ntfs_dir_open`, `fs_ntfs_dir_next`, `fs_ntfs_dir_skipped` and
  `fs_ntfs_dir_close` that copies each borrowed `fs_ntfs_dirent_t` before
  the next call, bounds `name_len` by the name buffer, refuses an embedded
  NUL, raises on a non-zero errno, and refuses use after close; closing a
  volume closes its open iterators first. `Volume.list_dir` raises
  `IncompleteListingError` when the library reports skipped index entries
  rather than returning a partial listing as complete. CI compiles an
  independent C caller and requires the Python names, types and record
  numbers to match it on the same fixture. A Windows producer now captures
  controlled ASCII, Unicode and empty directory listings, unchanged native
  file IDs and native FILE records on that same image. The consumer checks
  both Python and C against the image-hash and exact-head-bound record,
  validates the full-reference-to-ordinal bridge against Windows bytes,
  and requires zero skipped tests and index entries. Exact-head native
  and full-suite green evidence remains outstanding.
