### Fixed

- **A failed copy-repair copy leaves no target.** `repair_plan::copy_repair`
  and `fsck.ntfs --apply PLAN --to TARGET` copy under a hidden name beside the
  target and move the copy to the target path once it is complete and flushed,
  so a copy that fails leaves nothing there and can be run again.

### Security

- **A copy-repair target is private.** On Unix a target is created with mode
  0600 whatever its source's mode, so a copy of a volume cannot be read by
  users who could not read the source.
