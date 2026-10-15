### Changed

- **Pull requests run the quick checks; the merge queue runs the full pipeline.** Every pull request, each layer of a stack included, and every push to main run the unit, script, lint and semver checks on Linux. The merge queue, tags, manual dispatches and any unexpected event run every tier: the ARM64 and macOS legs, the Windows fixtures and their readers, the Windows format gate when format-relevant code changes, and the kernel oracle. `ci-ok` refuses a run whose depth decision is missing or invalid, and on a quick run excuses only the full jobs' skips.
