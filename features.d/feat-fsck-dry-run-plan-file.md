## Checking and recovery

| `fsck.ntfs --dry-run --plan FILE` / `repair_plan::save_plan`: a dry run's repair plan saved to an external file for `--apply`, the volume opened read-only and never written | Supported; FILE naming the volume, FILE naming a file that exists, and `--plan` without `--dry-run` are refused before anything is written; the saved plan loads back and matches the report's plan on every interrupted capture | Unreleased (#906) | #710, #905 | `windows_interrupted_fixture.rs` |
