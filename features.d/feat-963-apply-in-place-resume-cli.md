## Checking and recovery

| `fsck.ntfs --apply PLAN --in-place --resume --journal FILE`: resume a stopped in-place repair from the journal it left | Supported; applied units are skipped, the open unit redone and the rest applied, and a `--resume` naming a missing journal or the volume itself is refused before anything is written; a repair stopped halfway reaches what Windows' own restart recovered on every interrupted capture | Unreleased (#963) | #958 | `windows_interrupted_fixture.rs` |
