### Changed

- **`fs.ntfs` reports a missing path with the `not_found` error kind
  (#1191).** `ls`, `ls -R`, `stat`, `read` and `export` given a path that is
  not there, or one whose parent is not there, still exit 1 and now carry
  `"kind": "not_found"` beside `code` and `error` in the JSON error. A
  script can match that across drivers, whatever the message says. Only
  path resolution is reported this way, so metadata missing part-way
  through a read is still an ordinary failure.
- **Built on rust-fs-core 0.4.2**, for `CliError::not_found`.
