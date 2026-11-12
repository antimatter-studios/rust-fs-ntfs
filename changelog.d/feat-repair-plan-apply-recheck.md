### Added

- **Applying a repair plan re-checks every unit on the target immediately before writing it (#914).** `repair_plan::apply_plan_units`, and so `repair_plan::copy_repair`, checks each unit with `repair_plan::check_unit_on_device` at its turn; a unit whose ranges no longer hold their before-images stops the apply before it with `CopyRepairError::Target`, naming the unit, the write and its byte range, and every earlier unit stays written and flushed. It is behind the `plan-file` feature.
