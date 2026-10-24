### Fixed

- **A rename of a name held in an extension record keeps that name's
  namespace in the rebuilt index entry (#785, part of #662).** A rename to a
  name of another length rebuilds the parent's index entry, and took the
  old name's namespace from the base record alone. A name held in an
  extension record, such as the WIN32 half of a WIN32 + DOS pair, got the
  namespace worked out from the new name instead, so the directory and the
  file disagreed. The namespace is now read from whichever record holds the
  name.
