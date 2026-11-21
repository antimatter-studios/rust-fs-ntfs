## Checking and recovery

| Change journal records from a read-only `fs_core::BlockRead` device (`evidence::usn_journal_state`, `evidence::read_usn_records`) | Supported: every record compared with `fsutil usn readjournal` on a journal Windows wrote | Unreleased (#1318) | #1318 | `tests/journal_evidence_readonly.rs` |
| `$LogFile` state, bytes and record walk from a read-only `fs_core::BlockRead` device (`evidence::logfile_state`, `read_logfile`, `walk_logfile`) | Supported: fifteen Windows captures taken mid-write | Unreleased (#1307) | #1307 | `tests/journal_evidence_readonly.rs` |
| `$LogFile` replay held in memory over a read-only device (`evidence::replay_logfile_in_memory`), readable as the recovered volume | Supported: on fifteen Windows captures the view is byte for byte what `fsck`'s replay writes, and its files are the ones Windows listed after its own restart pass; the device is unchanged | Unreleased (#1307) | #1307 | `tests/journal_evidence_readonly.rs` |
