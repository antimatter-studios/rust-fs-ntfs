### Fixed

- **A freshly formatted volume's `$Extend\$Quota` holds what Windows
  formats (#862).** mkfs left `$Q` empty at 4K clusters and wrote only
  the quota control entry below that, with an empty `$O`. It now writes
  what two Windows-formatted volumes hold at 4K and 1K clusters: the
  control entry (owner id 1) and the default owner's entry (owner id
  0x100, BUILTIN\Administrators) in `$Q`, and that owner's SID in `$O`,
  with each `$Q` entry's change time set to the format time. A Windows
  scenario grading these volumes is #1111.
