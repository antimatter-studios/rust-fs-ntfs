### Fixed

- **Writing or deleting a named stream moves its file's quota charge (#851,
  part of #677).** `write_named_stream` and `delete_named_stream` changed a
  file's allocation without moving what it is charged, so on a volume with
  quota tracking on its owner's usage in `$Extend\$Quota:$Q` drifted from
  what Windows counts. Both now charge the file its MFT record, a separate
  record-sized amount for each named stream even when resident or empty,
  and the allocated size of each non-resident `$DATA` stream, and move its
  owner's usage by the difference. The resident-only named-stream writer
  uses the same accounting. Windows measurements with 1024- and 4096-byte
  file records establish the per-stream charge.
