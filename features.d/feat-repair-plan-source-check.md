## Checking and recovery

| `repair_plan::check_plan_source`: a loaded plan's source size, volume serial and every write's before-image, including earlier overlapping writes | Supported; the first mismatch is refused without writing to the source | Unreleased (#912) | #711 | `windows_interrupted_fixture.rs`, `repair_plan_source.rs` |
