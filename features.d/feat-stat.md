## Interfaces

| `fs.ntfs stat`: one path's record and sequence, type, links and their names, size, allocated bytes, attributes, reparse tag, object ID, stream and EA counts, and all four times to 100 ns, links reported rather than followed (`--features cli`) | Supported; graded against what Windows reports for each path of a volume it wrote | Unreleased | #642 | `tests/cli/test-stat.sh`, `tests/cli-oracle/test-stat-windows.sh` |
