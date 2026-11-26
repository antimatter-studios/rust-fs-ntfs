### Fixed

- **A dropped exclusive open releases its device lock.** In-place repair now
  unlocks the device and journal explicitly, so a process the host spawns at
  the same moment cannot keep them reported as held by another process.
