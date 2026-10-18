### Added

- **A repair journal read back reports which units an in-place apply marked applied and which it left open (#926).** `repair_journal::progress` reads a journal against its plan and returns the units marked applied and the unit whose intent has no applied record, the point a resume starts from. An intent record whose writes are not exactly its plan unit's is refused, naming the unit and write. It is behind the `plan-file` feature.
