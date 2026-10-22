### Fixed

- **A zero-length `write_at` to a path that does not exist is refused**
  (#1168). It returned `Ok(0)` before the path was looked up, so writing
  nothing to a missing file succeeded. The path now resolves first, and a
  write of nothing to a file that exists is still a no-op that leaves the
  image untouched.
