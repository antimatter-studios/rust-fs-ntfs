## Interfaces

| `fs.ntfs` reading verbs (`ls`, `ls -R`, `stat`, `read`, `export`) on a missing path: exit 1 with `"kind": "not_found"` in the JSON error, as the CLI contract has it | Supported | Unreleased (#1191) | #1191 | `tests/cli/test-fs.sh`, `test-stat.sh`, `test-ls-recursive.sh` |
