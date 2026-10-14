### Added
- Reparse points and object ids can be inserted into their `$Extend` view indexes after the indexes spill into INDX blocks, with full roots promoted and full leaves split under `COLLATION_NTOFS_ULONGS` (#571).
- Object id uniqueness is checked across the whole index, including entries in INDX blocks.
