### Added

- **A repair plan unit is checked on the device about to receive it (#913).** `repair_plan::check_unit_on_device` reads the device as it stands at that unit's turn, the plan's earlier units already written, lays only the unit's own earlier writes over it, and refuses the unit when a write's range does not hold its before-image, naming the unit, the write and its byte range. Nothing is written. It is behind the `plan-file` feature.
