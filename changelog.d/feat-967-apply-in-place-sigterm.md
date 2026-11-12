### Added

- **`fsck.ntfs --apply PLAN --in-place --journal FILE` also stops at a unit boundary on SIGTERM (#967).** SIGTERM, what a service manager, a shutdown or `timeout` sends, is handled as SIGINT is: the apply, or a `--resume`, finishes the unit in hand, names the units applied and exits with fsck(8)'s operational status (8), leaving a journal that `--resume` finishes.
