### Changed

- **An in-place repair audits the whole repaired volume after its last unit (#941).** `apply_in_place` and `resume_in_place` (and their `_observed` forms) now return the `fsck::check_io` report of the device as it reads after the last unit, instead of `()`. The audit writes nothing, so the dirty flag the repair set stays set and the report shows it; any other finding is one the repair left. The audit's scope is `check_io`'s, not a structural check of the whole volume.
