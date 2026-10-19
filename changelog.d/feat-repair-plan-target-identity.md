### Added

- **Applying a repair plan checks the written device's size and serial before the first unit (#915).** `repair_plan::apply_plan_units`, and so `repair_plan::copy_repair`, checks the target with the new `repair_plan::check_device_identity` before anything is written: a target whose size in bytes or boot-sector volume serial is not the plan's source's is refused with `CopyRepairError::Target`, naming the size or serial, and no unit reaches it. It is behind the `plan-file` feature.
