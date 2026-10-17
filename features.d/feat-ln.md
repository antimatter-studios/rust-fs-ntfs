## Interfaces

| `fs.ntfs ln`: a hard link through the library's `link`, in any directory, and with `-s` a symbolic link whose target is the text as given, relative, from the root or a drive path; a hard link to a directory, a file at Windows' 1024 links and a name already there as Windows compares names refused, and every refusal made before the image is opened for writing (`--features cli`) | Supported; a symbolic link is a file link, never a directory link (`mklink /D`) | Unreleased | #646 | `tests/cli/test-ln.sh`, `cli-link-win-verify-chkdsk` |
