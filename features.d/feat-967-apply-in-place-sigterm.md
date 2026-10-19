## Checking and recovery

| `fsck.ntfs --apply PLAN --in-place --journal FILE`: SIGTERM stops at a unit boundary | Supported; SIGTERM finishes the unit in hand, names the units applied and exits 8, as SIGINT does, leaving a journal that `--resume` finishes into what Windows' own restart recovered on every interrupted capture | Unreleased (#967) | #958 | `windows_interrupted_fixture.rs` |
