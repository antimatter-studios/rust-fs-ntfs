### Added

- **`fsck.ntfs --apply PLAN --in-place --journal FILE` reports progress and stops on SIGINT at a unit boundary (#964).** While it applies, and while `--resume` continues, each unit marked applied prints a `progress: U of N units, B of T bytes` line on stderr. A SIGINT is seen before the next unit begins, so the unit in hand is finished first; the apply then names the units applied and exits with fsck(8)'s operational status (8), leaving a journal that `--resume` finishes.
