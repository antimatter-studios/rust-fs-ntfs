### Added

- **`fsck.ntfs --apply PLAN --in-place --resume --journal FILE` resumes a stopped in-place repair (#963).** FILE is the journal the stopped apply left: units it marks applied are skipped, the open unit is redone and the rest are applied, through the library's `resume_in_place`, which checks the journal against the plan and the volume before writing. `--resume` naming a journal that does not exist, or the volume itself, exits with the usage status and writes nothing. Without `--resume` an existing journal is still refused.
