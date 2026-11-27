### Fixed

- `truncate_by_record_number` resizes a resident unnamed `$DATA`: it cuts, zero-extends and promotes it, and refuses a compressed, encrypted or sparse one (#1398).
