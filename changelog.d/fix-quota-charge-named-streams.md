### Fixed

- **Writing or deleting a named stream moves its file's quota charge (#851,
  part of #677).** `write_named_stream` and `delete_named_stream` changed a
  file's allocation without moving what it is charged, so on a volume with
  quota tracking on its owner's usage in `$Extend\$Quota:$Q` drifted from
  what Windows counts. Both now charge the file its MFT record plus the
  allocated size of each of its non-resident `$DATA` streams, named or
  not, and move its owner's usage by the difference, as rewriting its
  unnamed stream already did.
