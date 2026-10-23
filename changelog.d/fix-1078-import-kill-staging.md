### Fixed

- **An `fs.ntfs import` killed before its rename no longer leaves a staging file nothing removes (#1078).** The staging name is one per destination, `.dj-import-<hash>.partial` beside PATH, and the next import of the same PATH removes the file a killed one left there before it stages again. A directory at that name is refused rather than removed.
