## Checking and recovery

| `PrimitiveAPI` salvage reads of streams with an `$ATTRIBUTE_LIST`, a non-zero first VCN or LZNT1 compression | Supported: compared with the SHA-256 Windows reads for each stream, and with the independent `ntfs` reader for a run list split across records; a missing, torn, free or foreign extension record and an undecodable compression unit are refused by name | Unreleased (#1379) | #1379 | `tests/salvage_streams_windows.rs` |
| `PrimitiveAPI` hard-link names held in extension records (`ResolvedRecord::file_names`) | Supported: compared with `fsutil hardlink list` | Unreleased (#1379) | #1379 | `tests/salvage_streams_windows.rs` |
| `PrimitiveAPI` salvage reads of encrypted streams | Refused by name | Unreleased (#1379) | #1379 | `src/primitives/extents.rs` |
