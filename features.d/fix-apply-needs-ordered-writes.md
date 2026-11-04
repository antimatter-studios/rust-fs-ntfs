## Checking and recovery

| `repair_plan::apply_plan_units` on a target that cannot order its writes (`can_sync() == false`) | Refused before any write or flush, including an empty plan; the error names #518 | Unreleased (#1281) | #1253 | `repair_plan_copy.rs` |
