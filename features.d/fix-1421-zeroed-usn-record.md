## Checking and recovery

| Zeroed bytes in `$UsnJrnl:$J` | Partial: refused as `JournalReadError::ZeroGap` unless they end a 4096-byte page and the record after them is longer than they are; a loss of that exact shape reads as padding | Unreleased (#1423) | #1421 | `tests/journal_evidence_readonly.rs`; `tests/usn_image_windows_oracle.rs` |
