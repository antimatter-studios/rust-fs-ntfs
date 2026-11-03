### Added

- **Repair journal records encode an append-only external journal (#918).** Behind `plan-file`, the record encoders bind a header to a plan and source, record each unit's preimages and redo, and mark applied units. The version 2 format verifies each frame's length before identifying a torn tail (#1251), then verifies the complete record's SHA-256 and ordering. The caller owns file placement, appending, torn-tail truncation, flushing and syncing; the encoders themselves provide no durability guarantee. See `docs/repair-journal-file.md`.
