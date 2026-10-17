## Interfaces

| `fs.ntfs ls --recursive`: byte-sorted iterative depth-first paths and depths, finite exact-fit bounds, partial reports on failure, reparse points never followed, hard-link names retained and revisited directory records not entered (`--features cli`) | Supported; graded against Windows' own recursive enumeration and file IDs | Unreleased | #644 | `tests/cli/test-ls-recursive.sh`, `tests/cli-oracle/test-ls-recursive-windows.sh`, `cli_ls_recursive_faults.rs` |
