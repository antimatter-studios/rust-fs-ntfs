## Checking and recovery

| Repair journal byte format: plan/source binding, intent and applied records, checksummed frame lengths and records, whole-prefix recovery after a torn tail | Supported; `plan-file`; file I/O, external placement and durability remain the caller's responsibility | Unreleased (#924) | #918, #1251, #713, #919 | `tests/repair_journal.rs`, `tests/windows_interrupted_fixture.rs` |
