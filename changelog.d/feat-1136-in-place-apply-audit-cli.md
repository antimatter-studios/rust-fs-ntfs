### Changed

- **`fsck.ntfs --apply PLAN --in-place` reports the audit of the repaired volume (#1136, part of #943).** The report now carries an `audit` object with the dirty flag, `$LogFile`'s state and every finding `check_io` makes of the volume once the last unit is applied, with or without `--resume`. The dirty flag the in-place repair leaves set is expected and the exit status stays 0; any other finding exits 4, fsck(8)'s "errors left uncorrected".
