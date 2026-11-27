### Added

- **A salvage plan can carry the file's name and times.** `NtfsAdapter::plan_metadata` returns the candidate's name and the base record's created, modified and accessed FILETIMEs, in decimal, for `Plan::metadata`. Times are left absent when zero, and when the record cannot be read, does not validate or has another sequence number than the candidate's (#1390).
