## Checking and recovery

| `repair_plan::save_plan` / `load_plan`: a repair plan as a versioned, checksummed file bound to its source's size and volume serial number, refused when truncated, changed anywhere or of another format version | Supported in the library, behind the `plan-file` feature; a plan saved from each Windows interrupted capture, loaded back and applied, reaches what Windows' own restart recovered | Unreleased (#907) | #905 | `windows_interrupted_fixture.rs`, `repair_plan_file.rs` |
