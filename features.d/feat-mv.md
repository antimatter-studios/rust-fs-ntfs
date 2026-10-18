## Interfaces

| `fs.ntfs mv`: a rename within one directory through the library's rename, same length, longer, shorter or case only, files and directories; a cross-directory move refused with exit 3, a name already there refused as Windows compares names, and every refusal made before the image is opened for writing (`--features cli`) | Supported within a directory; a move across directories is not implemented | Unreleased | #645 | `tests/cli/test-mv.sh`, `cli-rename-win-verify-chkdsk` |
