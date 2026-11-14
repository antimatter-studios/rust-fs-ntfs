## Checking and recovery

| A log holding an operation no capture has held, a prepared or committed transaction not forgotten, a checkpoint dirty-page entry that describes no page, or an LFS version other than 2.x | Refused for writing, with nothing written, naming the open issue for its shape | Unreleased (#1160) | #650, #651, #652, #653, #654, #1158, #1159 | `windows_interrupted_fixture.rs` |
| Interrupted-capture classification distinguishes replay refusal from damage | Partial: classifier reports remaining findings; full replay-oracle validation pending | Unreleased (#1341) | #1307 | `tests/scripts/classify-interrupted-captures.sh` |
| Windows restart comparison of undo-touched index blocks | Windows-authored block sizing covered; full restart comparison pending | Unreleased (#1341) | #756 | `tests/undo_index_compare.rs`, `examples/undo_index_compare.rs` |
| `$LogFile` replay of a record that wraps onto the first page of the record area, leaving only its continuation there | Supported | Unreleased (#1341) | | `windows_interrupted_fixture.rs` |
| `$LogFile` replay of `UpdateResidentValue` over a compressed file's non-resident `$DATA` header; a compressed file read past its initialized size | Supported | Unreleased (#1341) | | `windows_interrupted_fixture.rs` |
