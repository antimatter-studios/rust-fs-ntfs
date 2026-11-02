### Added

- **A loaded repair plan is checked against its source before it is applied (#908).** `repair_plan::check_plan_source` reads the source and refuses it when its size or boot-sector volume serial number is not the plan's, or when it does not hold a write's before-image at that write's turn (the source's bytes under the plan's earlier writes), naming the unit and write. Nothing is written. It is behind the `plan-file` feature.
