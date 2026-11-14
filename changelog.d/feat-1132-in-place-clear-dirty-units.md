### Changed

- **An in-place plan asked to clear the dirty flag ends with marked clear-dirty units (#1132).** `plan_in_place_io` with `FsckOptions::clear_dirty` keeps the flag set through the repair's own units and ends the plan with `clear_dirty_io`'s writes, which `RepairPlan::clear_dirty_from` and `SavedPlan::clear_dirty_from` name; without it the plan clears nothing. The plan file is now format version 2, carrying that mark after the volume serial number; version 1 files still load, with no clear-dirty units.
