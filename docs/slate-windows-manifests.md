# Windows read manifests for Slate

The advisory Slate tier's Windows builder writes `windows-read/<case>.json`
for every image in `cases.tsv`. The manifest records what Windows reads from
an independent disposable wrapper of the original image. Windows' mount-time
changes are discarded; the image retained for Slate is hashed before and after
capture. The manifest is only published after successful enumeration, hashing
and verified wrapper cleanup.

Schema 1 has `case`, `image_sha256`, `oracle`, `capabilities` and `entries`.
The case and SHA-256 must match the same row in `cases.tsv` before any consumer
compares entries. A hash of a different image is never valid evidence.

Each entry has a volume-relative `path` starting with `/`, a `kind` of `file`
or `directory`, and a `streams` array. Every stream has a `name`, logical
`size` in bytes and lowercase SHA-256. The unnamed stream uses the empty name.
An empty file still has its unnamed stream with size zero and the SHA-256 of
zero bytes. Directories have no unnamed data stream, but their named streams
are captured. Paths and stream names use ordinal ordering, preserve case and
Unicode, and are serialized as UTF-8 JSON without a BOM.

The root directory itself is implicit; its streams are outside this schema.
The exact root metadata and mount-created names omitted are recorded in
`capabilities.excluded_root_names`. The same names below a user directory are
not excluded. All other entries, including hidden files, are captured.
Reparse points fail by path instead of being followed or silently omitted.
An enumeration, stream-open, length or hash error also fails by path.

`tests/scripts/slate-windows-manifest.ps1` creates ordinary files and named
streams through Windows and requires eleven contracts to execute. The advisory
workflow runs it on `windows-latest` before building its image cases. No NTFS
reader from this crate or Slate supplies expected paths or bytes.

This is the capture slice (#856) of #689. Slate extraction and comparison are
#857; the Windows-authored CLI image, complete writer coverage and read-case
floor are #858. The existing seven-case checker floor is unchanged because
this slice adds no images or Slate comparisons.
