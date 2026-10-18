### Added

- **An in-place repair refuses a journal location that is missing or on the device being repaired, before anything is written (#921).** `repair_journal::check_journal_location(journal, target)` refuses, naming the reason, a journal whose directory does not exist, a journal that is the target by path or as a hard link or symlink to it, a journal whose directory is on a filesystem the target device holds (its `st_dev` is the device's `st_rdev`), and a journal path where something already exists. Nothing is created at the journal path and nothing is written to the target either way.
