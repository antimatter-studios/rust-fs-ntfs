### Added

- `fs.ntfs TARGET read --stream NAME PATH` reads one named data stream using
  Windows case-insensitive name lookup, the existing chunked reader and
  `-o FILE` atomic output. A missing stream fails without stdout; plain
  `read` still reads unnamed data, and colon suffixes never select streams
  (#758, part of #657).
