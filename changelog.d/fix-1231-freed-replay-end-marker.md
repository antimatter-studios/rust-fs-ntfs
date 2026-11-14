### Fixed

- **A `$LogFile` replay that rewrites a non-resident attribute's header
  keeps the record whole** (#1231). Windows logs `UpdateResidentValue` over
  the 72-byte header of a compressed file's non-resident `$DATA`; the replay
  read that header's lowest VCN as a resident value's offset and length,
  took the write for a value growing to 72 bytes and shrank the attribute by
  8. `fsck -y` and `mount_rw_replaying_log` then left the records they
  rebuilt with no end marker, or no `FILE` signature, on every volume
  Windows was writing compressed files to when it stopped. The header is
  now overwritten in place, as Windows' restart does.
- **A compressed file reads as zeros past its initialized size** (#1238).
  Its compressed units were decompressed to the end of its data, so a file
  Windows was still writing when it stopped read back bytes Windows reads
  as zeros. Uncompressed files already read that way.
