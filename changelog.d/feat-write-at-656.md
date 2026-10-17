### Added

- **`fs.ntfs write --at BYTES PATH` patches an existing regular file.** The
  driver's positioned writer preserves the prefix and suffix, grows past EOF
  with a zero-filled gap, and leaves the file unchanged for empty input.
  Predictable refusals are rehearsed before a writable open; `--offset`
  remains the device offset. Reports include path, position, bytes and size.
