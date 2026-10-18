### Added

- **A repair plan saves to a file of its own and loads back only when intact (#905).** `repair_plan::save_plan` writes a plan as a versioned file bound to its source's size and boot-sector volume serial number, with the SHA-256 of every unit's before-image and a SHA-256 over the whole file; `repair_plan::load_plan` refuses a file that is truncated, longer than it says, of a newer format version, or changed anywhere, naming why. Both are behind the `plan-file` feature, which `cli` turns on. The format, and what a reader must refuse, is `docs/repair-plan-file.md`.
