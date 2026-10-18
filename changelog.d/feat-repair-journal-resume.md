### Added

- **A stopped in-place repair resumes from its journal (#927).** `repair_journal::resume_in_place` continues an in-place apply on the same device and journal: units the journal marks applied are read back and skipped, never rewritten; the open unit is redone from its redo when each of its ranges holds its whole preimage or redo, without a second intent record; and every later unit is applied as before. A torn final record is dropped, and a journal holding no whole record starts again from its header. It is behind the `plan-file` feature.
