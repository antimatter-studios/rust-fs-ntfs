### Fixed

- **A file whose object id or reparse point sits past its index's root can
  be unlinked (#571).** Once Windows grows `$Extend\$ObjId:$O` or
  `$Extend\$Reparse:$R` past its root into INDX blocks, as it does on a
  volume with more than a handful of object ids, unlinking such a file,
  removing such a directory, or removing the attribute was refused. The
  entry now comes out wherever it sits: cut from a leaf, or, in an interior
  node, replaced by its neighbour as a directory's entries are. Putting a
  new entry into an index that has spilled is still refused, before
  anything is written.
