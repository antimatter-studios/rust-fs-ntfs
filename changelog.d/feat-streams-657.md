### Added

- `fs.ntfs TARGET streams PATH` lists named data streams with their logical
  and allocated sizes, residency, sparse and compression flags, as JSON or
  `--text`. It includes directory streams and streams in extension records,
  excludes unnamed data, and opens the image read-only (#757, part of #657).
