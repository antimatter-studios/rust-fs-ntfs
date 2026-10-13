### Fixed

- **Reparse points and object ids are entered in their `$Extend` indexes
  (#478).** Writing a reparse point or a symlink, or an object id, never
  touched `$Extend\$Reparse:$R` or `$Extend\$ObjId:$O`, the indexes Windows
  enumerates them through and chkdsk keeps in step with them. Each write
  now adds the file's entry, a retag or a new id moves it, and removing the
  attribute, unlinking the file's last name or removing the directory takes
  it out. A second file is refused an object id another already has, as
  Windows refuses it. Only an index that still fits its `$INDEX_ROOT` is
  changed: one that has spilled into INDX blocks is refused before anything
  is written.
