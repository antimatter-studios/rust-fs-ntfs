### Added

- `fs.ntfs TARGET import SRC PATH [--verify]` imports one regular local file,
  or stdin, through core's shared bounded transfer into a staging name beside
  PATH and renames it to PATH only when complete (#842). An existing PATH is
  refused and `--replace` exits 3; tree import is tracked in #843.
