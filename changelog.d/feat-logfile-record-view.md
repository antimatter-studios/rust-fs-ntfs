### Added
- Read-only, bounded `$LogFile` record lookup by LSN reuses replay's page fixups, tail-copy selection and continuation handling. Captured Windows restart LSNs grade record identity; full-field independent decoding remains outstanding (#607, #620).
- The $LogFile comparisons read four pinned Windows checkpoint captures, so a commit gets the same verdict on every run; a daily workflow takes fresh captures and runs the same comparisons over them without gating merges (#620).
