# Python directory oracle

PR #630 adds an executable evidence path; native green evidence remains owed.
The existing Windows fixture job creates `/oracle-dir` on `ntfs-cli-read.img`,
with ASCII and Unicode files and two empty directories. `Get-ChildItem`
records names and types; `fsutil file queryfileid` records each full native ID
and its unchanged output. No system or arbitrary root entries are queried.
The ABI's synthesized `.` and `..` entries are graded against explicit native
self/parent queries; the volume root is queried only as `/oracle-dir`'s parent,
without enumerating its contents. Empty native listings have only those two
ABI entries.
The JSON sidecar binds the detached raw image SHA-256 to the checkout's exact
`git HEAD`. The consumer refuses a different image or revision before reading.

The C ABI returns an MFT ordinal, not a full Windows file reference.
[Microsoft's MFT_SEGMENT_REFERENCE](https://learn.microsoft.com/en-us/windows/win32/devnotes/mft-segment-reference)
defines the 48-bit segment number and 16-bit sequence. The producer preserves
the complete ID and separately asks Windows for that ordinal through
[FSCTL_GET_NTFS_FILE_RECORD](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_get_ntfs_file_record).
The complete native output buffer is retained as hex. The consumer requires
the returned ordinal to equal the requested ordinal (Windows can return a
lower one), and reconstructs the full ID using that ordinal and the
[FILE record header's sequence](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-record-segment-header).
It also checks native enumeration types against the native header flags.
This validates the representation bridge independently of this driver;
unexpected high bits, sequences or returned ordinals fail rather than vanish.

`tests/python/test_dir_listing_windows.py` compares both the Python iterator
and a compiled C caller against every controlled listing, including empties,
requires zero skipped index entries, and checks that the image stays unchanged.
Negative controls change names, types, native sequence bits, native returned
ordinals, image and head. Its runner requires all three tests and zero skips.
The existing `python-volume-info-windows` job runs this consumer too and stays
required by `ci-ok`; the existing revision-binding and aggregate guards remain.

Run the full `CI` workflow with `workflow_dispatch` at the pushed branch head.
The producer artifact contains the image and `ntfs-cli-read.directory.json`;
the consumer uses `FS_NTFS_EXPECTED_HEAD=${{ github.sha }}` for the same checkout.
No successful native run is claimed until this exact-head route and the full
required gates complete with zero ignored tests.
