### Changed

- **The Windows native fixture builder names the phase it is in.** Each
  fixture prints one timestamped line per lifecycle boundary (image, init,
  mount, format, populate, sync, cleanup), names the failing phase on error,
  and reports whether its owned VHD is still attached after cleanup.
