### Added

- `logfile_replay::walk_records` walks `$LogFile` records in LSN order,
  read-only. By default it starts where replay's analysis does, at the
  in-use client's checkpoint start; otherwise from a given LSN. Each
  record comes with its header view, its byte offset and the next LSN, read
  through replay's own page model. The walk is bounded by a record count
  and a per-record byte budget. A record that cannot be read ends it with
  the reason named and everything before it kept; an LFS 1.x log is
  refused as lookup refuses it (#607).
