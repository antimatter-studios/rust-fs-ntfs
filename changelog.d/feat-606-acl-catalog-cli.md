### Added

- `fs.ntfs TARGET acl catalog [--id ID] [--jsonl]` lists every copy in
  `$Secure:$SDS` in stream order, read-only. Each copy is reported with:
  - its physical offset and the logical offset its header claims;
  - its security id, size, and stored and computed hash, with whether each
    verifies;
  - for a primary, whether its mirror 256 KiB on is the same, different or
    missing;
  - its descriptor, decoded as `acl show` decodes one.

  `--id` keeps one security id's copies, and `--jsonl` prints one compact
  object a line. A malformed entry ends the walk: the copies before it are
  printed, and the command exits 1 naming its offset (#606).
