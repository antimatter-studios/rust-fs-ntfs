### Added

- A saved change-journal cursor is admitted against `USN_JOURNAL_DATA_V0` with its full 64-bit journal identity, and a reset journal, purged records, a cursor past `NextUsn` and contradictory journal bounds each report their own state instead of an empty batch (#609).

### Fixed

- The Windows test harness lets Windows detach a scenario's VHD before copying the volume back into the scenario image from the wrapper file, with no forced dismount and no offline/online cycle, and a detach that fails fails the copy instead of producing an image (#609).
