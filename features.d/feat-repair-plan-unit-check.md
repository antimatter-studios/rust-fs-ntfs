## Checking and recovery

| `repair_plan::check_unit_on_device`: each unit's before-images on its target device at its turn, including earlier overlapping writes within that unit | Supported; the first mismatch names the unit, write and byte range without writing or flushing | Unreleased (#916) | #913 | `windows_interrupted_fixture.rs`, `repair_plan_unit.rs` |
