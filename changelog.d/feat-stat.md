### Added

- **`fs.ntfs stat PATH` reports one path in full.** The record number,
  type, link count, size, allocated bytes, a link's target, and the access,
  modification, MFT change and creation times as seconds plus nanoseconds,
  in the shape every `fs.<fs> stat` shares. NTFS's own fields are under
  `ntfs`: the sequence number, the named attributes (unknown bits kept as
  hex), the security ID, the reparse tag, the object ID, the counts of named
  streams and EAs, and every `$FILE_NAME` with its parent, namespace and
  four times. NTFS keeps no POSIX owner, so `uid` and `gid` are null. A
  symlink or junction is reported rather than followed. Every field Windows
  reports is checked against Windows itself on a volume it wrote (#642).
