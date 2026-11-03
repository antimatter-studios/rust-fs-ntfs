## Checking and recovery

| `repair_plan::copy_repair`: a checked plan applied to a new target with a flush after every unit, the source opened read-only and never written | Supported; a source the check refuses leaves no target, a target that exists is refused and kept, and a failure once the target exists leaves it at its path, not a repaired volume; the target reaches what Windows' own restart recovered on every interrupted capture | Unreleased (#917) | #909 | `windows_interrupted_fixture.rs`, `repair_plan_copy.rs` |
