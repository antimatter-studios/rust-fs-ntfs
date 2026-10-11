### Fixed

- **A volume Windows is hibernated on is refused for writing** (#137).
  Hibernation and Fast Startup save the running kernel, NTFS's cached
  metadata included, in `hiberfil.sys`, and the next boot resumes from it,
  so Windows would not see anything written in between. Nothing read the
  file, and since the log cannot tell this case from a crash, a read-write
  mount or `fsck` replayed the log and wrote to the volume. Both now refuse,
  with nothing written, when `hiberfil.sys` begins `hibr` or `HIBR` or is
  too short to hold its header, as ntfs-3g does.
