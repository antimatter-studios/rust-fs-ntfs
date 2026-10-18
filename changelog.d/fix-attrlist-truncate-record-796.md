### Fixed

- **A truncate by record number resizes a listed file's unnamed stream
  through the `$ATTRIBUTE_LIST`.** It used to edit the base record alone,
  so a file whose unnamed `$DATA` lives in an extension record was refused
  or had the wrong attribute resized; it now goes through the list, as the
  path-based truncate does.
