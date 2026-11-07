### Changed

- **`fs.ntfs streams`, `xattr` and `acl show` report a missing path with
  the `not_found` error kind (#1194).** Given a path that is not there, or
  one whose parent is not there, they still exit 1 and now carry
  `"kind": "not_found"` in the JSON error, as `ls`, `stat`, `read` and
  `export` do. A missing EA keeps its own error and no kind.
