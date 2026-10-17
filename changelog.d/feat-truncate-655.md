### Added

- `fs.ntfs TARGET truncate PATH SIZE` changes a regular file's length in bytes, shrinking resident or non-resident data and zero-filling growth. Unsupported shapes and predictable failures are refused before a writable open, leaving the image byte-identical.
