### Added

- **`mft_inventory::inventory_batch_io` lists in-use MFT records in bounded,
  resumable batches (#609).** Each call examines at most the requested
  number of slots, unused ones included, returns each record's full file
  reference with its sequence number, and returns the first slot it did not
  read as an explicit cursor. A zero bound, a cursor past the table, a
  `$MFT` length that is not a whole number of records, and a table larger
  than a 48-bit reference can name are refused. Nothing is written.
- **`mft_inventory::inventory_batch_cancellable_io` stops a batch when its
  caller cancels (#609).** It asks before the first metadata read and before
  each record read, and reports `InventoryOutcome::Interrupted` with the
  first unread slot and any records already read, so an interrupted batch
  cannot be mistaken for a finished table and resumes with nothing lost or
  repeated.
