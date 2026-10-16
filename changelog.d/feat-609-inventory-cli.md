### Added

- **`fs.ntfs <target> inventory --max-slots N [--cursor SLOT]` prints one
  bounded, read-only MFT batch as JSON Lines (#609).** Each in-use record is
  a `record` line with its sequence number and its file and base-record
  references as decimal strings, so 64-bit values survive any JSON reader;
  each record that failed its fixup check is a `damaged` line; a final
  `batch` line reports slots examined, unused slots, `finished`, and the
  `next_cursor` that resumes the table. The bound counts unused slots, and
  successive batches are not an atomic view of a changing volume.
- **Ctrl-C or SIGTERM interrupts an inventory batch cleanly (#609).** The
  batch stops before its next record read, prints the records already read,
  and ends with a `batch` line carrying `"interrupted": true`,
  `"finished": false` and the first unread slot as `next_cursor`, so
  resuming from it neither loses nor repeats a record.
