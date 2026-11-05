## Checking and recovery

| `fsck.ntfs --apply PLAN --in-place --journal FILE`: progress on stderr, and SIGINT stops at a unit boundary | Supported; each applied unit prints units and bytes of the plan's total, and SIGINT finishes the unit in hand, names the units applied and exits 8, leaving a journal that `--resume` finishes into what Windows' own restart recovered on every interrupted capture | Unreleased (#964) | #958 | `windows_interrupted_fixture.rs` |
