### Added

- **A repair plan is applied by copy-repair, never writing its source (#909).** `repair_plan::copy_repair` opens the source read-only, checks it against the plan, and only then creates the target (refusing a path that already exists), copies the source into it and applies the plan's units there with `repair_plan::apply_plan_units`, which flushes after every unit and never inside one. A source the check refuses leaves no target behind. It is behind the `plan-file` feature.
