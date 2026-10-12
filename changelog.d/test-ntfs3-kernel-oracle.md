### Added

- **Linux's ntfs3 is an oracle: `chore test:kernel`** (#486). The kernel's
  own NTFS driver, a third implementation beside Windows and ntfs-3g, mounts
  volumes this crate wrote in the `fs-linux-test-harness` VM, and every path,
  size, SHA-256 and link target must match what was written, with nothing in
  ntfs3's kernel log. ntfs3 also writes a tree of its own, which this crate
  must read back byte for byte. A changed data byte must fail the
  comparison, and a dirty volume's kernel log line must reach it, so neither
  check can pass by checking nothing. The guest runs bookworm-backports'
  kernel, since Debian 12's own is built without ntfs3, and CI runs the tier
  on an x86_64 runner with KVM. The harness's tasks are `chore linux:*`.
