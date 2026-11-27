### Fixed

- Change-journal reads refuse zeroed bytes that are not the padding Windows leaves at the end of a 4096-byte page, with `JournalReadError::ZeroGap`, instead of skipping them and returning a shorter or empty batch (#1421). Zeroed records at the end of a page remain indistinguishable from padding when the next page starts with a record longer than all the zeros before it.
