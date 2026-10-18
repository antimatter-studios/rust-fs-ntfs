## Checking and recovery

| `fsck.ntfs --apply PLAN --to TARGET` / `repair_plan::copy_repair`: copy-repair of a volume into a new target by a saved plan, the source opened read-only and never written | Supported; a target that is the source, or that exists, is refused before anything is created, as is `--apply` beside `-y`, `-p` or `--dry-run`; the target reaches what Windows' own restart recovered on every interrupted capture | Unreleased (#910) | #711, #909 | `windows_interrupted_fixture.rs` |
