### Added

- **Salvage reads follow a record's `$ATTRIBUTE_LIST` and decode LZNT1.** `PrimitiveAPI::read_mft_map` reads where the file records are from `$MFT`'s own record (following its attribute list when it has one), and `MftMap::contiguous` states the hypothesis when that record is lost. `read_mapped_record` reads a record by number, `resolve_salvage_record` returns every attribute a base record's list names from whichever extension record holds it, with `streams()`, `stream(name)` and `file_names()` across all of them, and `read_resolved_stream_at` reads a joined stream, decoding LZNT1 units. An extension record that is missing, torn, free, or names another base record leaves the streams it held a part of incomplete, and an incomplete stream is refused naming why, never read short; an undecodable compression unit is an error, never zeros; encrypted streams are refused (#1379).

### Changed

- `read_salvage_stream_at` decodes an LZNT1-compressed stream held in one record instead of refusing it, and its refusal of a record with an `$ATTRIBUTE_LIST` names `resolve_salvage_record`. `AttributeEvidence` gains `attribute_id`, `last_vcn` and `compression_unit` (#1379).
