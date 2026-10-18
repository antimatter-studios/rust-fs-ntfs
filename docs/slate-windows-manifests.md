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

## Allocation manifests

On the same disposable mount, the builder also writes
`windows-alloc/<case>.json` (#875, part of #692). Schema 1 has `case`,
`image_sha256`, `oracle`, `volume`, `capabilities` and `entries`, bound to
`cases.tsv` exactly as the read manifest is.

`volume` holds `bytes_per_cluster`, `total_clusters`, `free_clusters` and
`reserved_clusters` from `FSCTL_GET_NTFS_VOLUME_DATA`, the control behind
`fsutil fsinfo ntfsinfo`. Each entry has the read manifest's `path` and
`kind`, and a `streams` array covering the same streams. Every stream has its
`name`, its `extents` from `FSCTL_GET_RETRIEVAL_POINTERS`, the control behind
`fsutil file queryextents`, and `allocated_clusters`, the sum of its
allocated extents. An extent is `vcn`, `clusters` and `lcn`; a null `lcn` is
a sparse or compressed hole. A resident or empty stream has no extents. The
controls are called directly, so no locale-formatted text is parsed.

The volume counts are Windows' counts after it mounted the read wrapper, so
they include the clusters of any mount-created root names the entries
exclude; `capabilities.volume_counts` says so. Reparse points and
unreadable streams fail by path.

`tests/scripts/slate-windows-allocation.ps1` formats a scratch volume with
Windows, writes a resident file, a 256 KiB file with a 64 KiB named stream,
a sparse file with a leading hole and a junction through Windows, and
requires fourteen contracts to execute. Get-Volume's free bytes, another
Windows API, must lie between the free and the unreserved free clusters. No
NTFS reader from this crate or Slate supplies expected values. Comparing
Slate's allocation with these records is #877.
