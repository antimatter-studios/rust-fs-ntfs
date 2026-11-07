### Added

- `fs_ntfs_read_security_descriptor(image, path, out_buf, out_buf_len,
  out_len)` reads a file's stored security descriptor through the C ABI.
  It is the file's own `$SECURITY_DESCRIPTOR`, else the `$Secure:$SDS` copy
  its security id names: a verified primary, else its verified mirror,
  never an unverified copy. It follows `fs_ntfs_read_reparse_point`'s size
  query: 1 copied, 2 the buffer is too small and nothing is written, -1 an
  error. `sds::file_descriptor` and `sds::file_descriptor_io` are the
  Rust side, and `fs.ntfs acl show` now reads through them (#606).
