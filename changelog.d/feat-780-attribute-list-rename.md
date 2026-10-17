### Fixed

- **Renaming a name of a file with an `$ATTRIBUTE_LIST` rewrites its
  `$FILE_NAME` wherever it lives (#780, part of #662).** `rename` looked for
  the `$FILE_NAME` in the base record alone, so a name held in an extension
  record, as Windows lays names out, was renamed in the directory and left as
  it was in the file. The name is now rewritten in the record that holds it,
  and one that no longer fits there once renamed moves through the list,
  with its list entry.
