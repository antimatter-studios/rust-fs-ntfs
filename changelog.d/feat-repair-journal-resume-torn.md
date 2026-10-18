### Added

- **A resumed in-place repair redoes a unit whose device write was torn in the middle (#928).** `repair_journal::resume_in_place` now redoes the open unit when a write's range holds part of its new bytes and part of its old ones, in any order, as a power cut during the write leaves it: a range counts as torn when every byte equals the same byte of the preimage or of what the unit leaves there. It is behind the `plan-file` feature.
