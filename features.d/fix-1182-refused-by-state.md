## Interfaces

| `fs.ntfs` namespace verbs (`rm`, `rmdir`, `mv`, `ln`, `mkdir`, `truncate`, `write --at`) refused by the volume's state (a directory that is not empty, a name already there, a missing target, a part of the path that is not a directory): exit 4, `refused`, as the CLI contract has it | Supported | Unreleased (#1182) | #1182 | `tests/cli/test-rm.sh`, `test-write.sh`, `test-mv.sh`, `test-ln.sh`, `test-truncate.sh`, `test-write-at.sh` |
