### Fixed

- **A file deleted on a volume that tracks quotas gives its charge back
  to its owner (#674, part of #510).** With quota tracking on, Windows
  charges each file to an owner, and `$Extend\$Quota:$Q` holds each
  owner's usage. Unlinking a file's last name, removing a directory and
  renaming over a file freed the record and left the owner charged for a
  file that was gone, which `chkdsk` reports. Each now takes the file's
  `quota_charged` off its owner's `$Q` usage, in the index root or in an
  index block. An owner with no `$Q` entry, or one charged less than the
  file, refuses the delete with nothing written. Creates do not yet
  charge a file (#676).
