### Added

- **`fsck.ntfs --apply PLAN --to TARGET` copy-repairs a volume into a new target (#910).** It loads the plan file and applies it through `repair_plan::copy_repair`: the volume is opened read-only and checked against the plan, and only then is TARGET created, the volume copied into it and the plan applied there. `--to` naming the volume itself (by path, or the same device and inode), `--to` naming a file that exists, `--apply` without `--to`, and `--apply` beside `-y`, `-p` or `--dry-run` are each refused with a message naming the reason, before anything is created.
