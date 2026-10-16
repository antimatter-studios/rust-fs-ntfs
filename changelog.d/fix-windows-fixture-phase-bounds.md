### Fixed

- **A Windows native fixture that stalls or fails cleans up after itself.**
  Each lifecycle phase now fails on its own once it runs past
  `FIXTURE_PHASE_TIMEOUT_SECONDS` (600 s by default), naming the phase; a
  native command that exits nonzero while populating fails the fixture; and
  on any failure the builder detaches the VHD and removes that run's partial
  image and scratch VHD, while a finished image is kept for its consumers.
  Cleanup and one recovery attempt each have their own
  `FIXTURE_CLEANUP_TIMEOUT_SECONDS` deadline (30 s by default); cleanup
  failure invalidates the image and never removes an attached scratch VHD.
