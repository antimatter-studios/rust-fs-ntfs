### Changed

- **A resumed in-place repair names the bytes changed while it was stopped (#929).** When something else changed the device while an in-place apply was stopped, `repair_journal::resume_in_place` refuses before writing anything to the device or the journal, and its `OpenUnitRange` or `DeviceReadBack` refusal now names the unit, the write and the bytes that changed, from the first to the last, rather than the write's whole range. It is behind the `plan-file` feature.
