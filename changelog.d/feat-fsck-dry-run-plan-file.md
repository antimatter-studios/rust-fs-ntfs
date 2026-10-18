### Added

- **`fsck.ntfs --dry-run --plan FILE` saves the repair plan to an external file (#906).** The dry run's plan is written through `repair_plan::save_plan` to FILE, bound to the volume's size and serial number, for `--apply` to load; the volume is opened read-only and the exit status is `-n`'s. FILE naming the volume itself (by path, or the same device and inode), FILE naming a file that exists, and `--plan` without `--dry-run` are each refused with a message naming the reason, before the volume is opened and with nothing written. A plan the repair refuses saves no file and exits 8.
