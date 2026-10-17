# Provenance

What this crate's knowledge of NTFS was taken from, what verifies it, and every
third-party byte the repository carries or once carried. The rule this record
serves is in `AGENTS.md` under "Source material and format evidence". This
file records what was actually done, and it is kept accurate rather than
aspirational: when a source is used, a citation corrected, or a binary added
or removed, this file changes in the same pull request.

## Sources

| Source | What it is | Used for |
|---|---|---|
| [Linux-NTFS documentation](https://flatcap.github.io/linux-ntfs/ntfs/) | Documentation of the on-disk format, by the Linux-NTFS project (© 1996–2004 Richard Russon and others, GNU FDL 1.3). It is prose and tables, not an implementation. | Most on-disk layouts: the file record, attribute header, `$STANDARD_INFORMATION`, `$FILE_NAME`, `$OBJECT_ID`, `$VOLUME_INFORMATION`, index entries and records, collation rules, `$AttrDef`, `$Secure`, `$Bitmap`, the boot sector, data runs, fixups and namespaces. |
| Microsoft's Win32 devnotes | Microsoft's published documentation of some on-disk NTFS structures: [ATTRIBUTE_RECORD_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header), [FILE_RECORD_SEGMENT_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-record-segment-header), [MULTI_SECTOR_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/multi-sector-header), [MFT_SEGMENT_REFERENCE](https://learn.microsoft.com/en-us/windows/win32/devnotes/mft-segment-reference), [ATTRIBUTE_LIST_ENTRY](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-list-entry), [Master File Table](https://learn.microsoft.com/en-us/windows/win32/devnotes/master-file-table), [STANDARD_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/devnotes/standard-information) and [FILE_NAME](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-name). | The fields each one names. They are partial: `STANDARD_INFORMATION` documents only OwnerId (0x30) and SecurityId (0x34), and `FILE_NAME` only the parent reference, the name length (0x40) and the flags byte (0x41, NTFS 0x01 and DOS 0x02). |
| [MS-FSCC](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/efbfe127-73ad-4140-9967-ec6500e66d5e) | Microsoft's open specification of file-system wire structures. | §2.1.2, reparse data buffers; §2.6, file attribute bits. Its §2.3 to §2.5 are FSCTL and information-class buffers, not on-disk records, and are not cited for on-disk layouts. |
| [`FILE_FULL_EA_INFORMATION`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/ns-wdm-_file_full_ea_information) | Microsoft's driver-kit documentation of an extended-attribute API entry. | The API entry fields; the on-disk `$EA` representation, including the final entry's nonzero aligned length, is compared with Windows-authored bytes by `ea_information_windows_oracle` (#745). |
| MS-DTYP §2.4.6 | Microsoft's specification of `SECURITY_DESCRIPTOR`. | The self-relative descriptors in `$Secure:$SDS`. |
| Microsoft's FAT specification | The BPB that NTFS's boot sector shares. | BPB field names in `mft_io.rs`. |
| Windows itself | `format.com` and `Format-Volume` output, Windows-written and Windows-recovered volumes, and `chkdsk` as the grader. | Every layout above is checked against it (next section), and some facts come only from it: the `$Secure` hash, view-index entry layout, attribute alignment, `$UpCase` and the clean `$LogFile`. |
| The [`ntfs` crate](https://crates.io/crates/ntfs) (MIT OR Apache-2.0) | An independent reader, used as a dependency of the fsck tests. | Building test state through APIs other than the code under test. |

ntfs-3g's tools and the Linux kernel's ntfs3 driver are run as subprocess
oracles. That is allowed, and their source is not opened.

[Slate](https://github.com/NullMagic2/slate-ntfs), a third NTFS
implementation, is a supplementary subprocess oracle
(`.github/workflows/slate-oracle.yml`, #687). Its core and tools crates
declare MIT and are built from the commit `chores.yml` pins; its
`ntfs-chkdsk --check` grades images Windows `chkdsk` has already graded, and
Windows' verdict is the reference. Its `ntfs_utils` crate (MIT AND
GPL-2.0-or-later, holding a table ported from ntfs-3g), `kernel/` and
`kernel/patches/` are never built, and nothing from Slate is in this crate.
What was read to wire it in: its licence files, Cargo manifests, `build.sh`,
README and `RECOVERY_TRANSACTIONS.md`, and for the checker's exit statuses
the usage text and exit constants of the MIT-licensed
`src/tools/bin/ntfs-chkdsk.rs`. None of it is a source of this crate's
knowledge of NTFS.

To locate the `InvalidSecurity` finding on a fresh volume (#837), the
conditions of Slate's MIT-licensed `src/ondisk/security_store.rs` at
`579dfeb` were read: it refuses a `$Secure` stream whose attribute carries any
flag. The layout the fix writes comes from Windows, not from Slate: a
Windows-formatted 256 MiB volume's `$SDS` is one contiguous run with flags 0,
and its `$Secure` is not marked sparse.

`$Extend`'s `$I30` keys for `$ObjId`, `$Reparse` and `$Quota` (#838) are
written from a Windows-formatted 256 MiB volume's bytes: each key carries the
child's `$FILE_NAME` times, zero sizes and attributes `0x20000006`. No Slate
source was read for this; Slate only reported the disagreement.

The rescue scan (`src/rescue.rs`, #720) follows the behaviour of Slate's
sector rescue, read in its MIT-licensed `src/tools/recovery_io.rs` at
`579dfeb` (its rescue, surface-scan and rescue-archive functions and the
code beside them, not the rest of the file): bulk reads retried one sector
at a time on a media error, and append-only checksummed records with
explicit unreadable spans. No code was copied, and the archive
format is this crate's own (`docs/rescue-archive.md`). The scan reads raw
sectors and interprets no NTFS structure, so this too is no source of the
crate's knowledge of the format.

## Citations in the source

The first source files (April 2026) cited the Linux-NTFS documentation page by
page. On 2026-05-03, `e4c2725` replaced those citations with references to
*Windows Internals* and MS-FSCC, and later code followed the substituted style.
Many of those references do not contain the fact they were cited for:
MS-FSCC §2.4 is *File Information Classes* (its §2.4.2 is
`FileAllInformation` and its §2.4.4 `FileAllocationInformation`), not
`$STANDARD_INFORMATION` or `$FILE_NAME`. #611 corrected the module headers to
Microsoft's devnotes, and #489 corrected the remaining inline citations in
`src/`, each to the source that does hold the fact:

- **Linux-NTFS documentation** for `$STANDARD_INFORMATION`, `$FILE_NAME`,
  `$OBJECT_ID` and `$VOLUME_INFORMATION` (layouts and flags), the collation
  rules (0x01 file name, 0x10 ULONG, 0x11 SID, 0x12 security hash, 0x13
  ULONGS), `$FILE_NAME` namespaces, `$AttrDef` entries, `$Secure`, and the
  boot sector's offsets.
- **Microsoft's devnotes** for the attribute record header, the non-resident
  length fields, the MFT record header fields they name, SecurityId, and the
  `$FILE_NAME` flags byte.
- **Measured on Windows**, because no documentation used here states it:
  - the view-index entry's data offset and length (`view_index.rs`, measured
    on `windows-interrupted-quota.recovered`);
  - quota accounting with tracking on: each file is charged its record plus
    its non-resident `$DATA` allocation, each owner's `$Q` usage is the sum
    of its files' charges, and most charges leave the entry's change time
    as it was (`write.rs`, `quota_charge.rs`, measured on
    `windows-interrupted-quota.recovered` and the log of
    `windows-interrupted-quota`);
  - the `$SDH`/`$SII` hash (`sds.rs`, from `Format-Volume` output);
  - 8-byte alignment of attribute headers and resident values: all 927
    attributes and 703 resident values in the 301 records of
    `windows-interrupted-1.recovered` are aligned.

**Unsupported, and recorded as such:** `fsck::reset_logfile` fills `$LogFile`
with `0xFF` as an empty log. Neither the Linux-NTFS documentation nor
Microsoft's describes that convention. A log Windows dismounts cleanly keeps
two restart pages and its checkpoint page, and only the rest is `0xFF`
(`test-disks/windows-clean-logfile.bin.gz`). #137 tracks replaying the log
rather than discarding it.

## What verifies each layout

The oracle is Windows. The tests named `*_windows_oracle` or
`*_windows_capture`, `windows_interrupted_fixture`, and the `chkdsk` matrix
compare this crate's reading or writing with Windows' own bytes or verdict.
The others exercise a layout over images that mkntfs or this crate made,
which shows only that the crate agrees with itself:

| Structure | Verified by |
|---|---|
| Boot sector, BPB | `boot_sector_geometry`, `mkfs_roundtrip` (Windows `chkdsk` on formatted images) |
| File record header, fixups | `mft_io`, `mft_inventory_windows_oracle`, `mftmirr_windows_oracle` |
| Attribute header, resident and non-resident | `check_io_reads_attribute_structure`, `read_attributes`, `data_runs` |
| `$STANDARD_INFORMATION` | `read_si_full`, `field_exhaustion_si`, `security_id`, `write_times`, `tests/cli-oracle/test-stat-windows.sh` (times and attributes against Windows) |
| `$FILE_NAME`, namespaces | `filename_fields_windows_oracle`, `rename_rederives_namespace`, `read_file_names`, `tests/cli-oracle/test-stat-windows.sh` (link names and parents against `fsutil hardlink list`), the `cli-rename-win-verify-chkdsk` matrix scenario (names `fs.ntfs mv` gave, read back by Windows and graded by `chkdsk`) |
| Indexes, collation | `index_name_collation`, `index_lookup_descent`, `fragmented_indx_fixture` and its matrix scenario (Windows enumeration and `chkdsk`); `tests/cli-oracle/test-ls-recursive-windows.sh` (paths, types, sizes and record numbers against Windows' `Get-ChildItem` and handles opened without following reparses) |
| View indexes | `extend_view_indexes`, `view_index_spilled` |
| `$Secure` | `security_descriptor_windows_oracle`, `security_id_must_exist` |
| `$EA`, `$EA_INFORMATION` | `ea_information_windows_oracle`, `ea_names_windows_oracle` |
| Reparse points | `readlink_windows_oracle`, `write_reparse` |
| `$OBJECT_ID` | `object_id`, `field_exhaustion_objid` |
| `$UsnJrnl` | `usn_journal_windows_oracle`, `usn_windows_capture` |
| `$LogFile` | `windows_interrupted_fixture` (replay compared with Windows' own recovery), `logfile_state` |
| `$UpCase` | `upcase` |
| Formatted volumes as a whole | the Windows `chkdsk` matrix (`test-matrix.json`) |

## Binary data in the published crate

Two files under `src/` are embedded with `include_bytes!` and ship in every
build. Both are captured Windows output, both are listed in `NOTICE`, and
`scripts/check-interrupted-bootstrap.py` refuses any file under `src/` that
holds an NTFS boot sector or a formatter's boot message.

- **`src/upcase-canonical.bin`** (128 KiB, SHA-256
  `41c26bc7a12bdaeb26025c93118697c7e3ef81ee048b00fe5cce2a472e0e0742`). The
  `$UpCase` table from a volume `format.com /FS:NTFS` made, captured with
  `fsutil file queryextents` and a raw read (`src/upcase.rs`). It is a
  functional mapping that NTFS's collation dictates: `chkdsk` rejects any
  other table. It is independently reproducible: on 2026-10-11 its 65,536
  entries were identical to a Windows-written volume's `$UpCase` and to the
  table `mkntfs` generates on its own, with no code point differing in any
  pairwise comparison (#490).
- **`src/logfile-canonical-12k.bin`** (12 KiB). The first three pages of a
  `$LogFile` Windows wrote, from which `mkfs` builds the clean log of a new
  volume (`src/mkfs.rs`, `LOGFILE_CANONICAL`). It holds log metadata, no
  code.

## Third-party code: none now, one file once

`src/boot-bootstrap.bin` held 426 bytes of Microsoft's NTFS boot-sector code
(its strings include `A disk read error occurred` and `BOOTMGR is
compressed`), and `format_filesystem` wrote it into every volume it made. It
was added in `6fc8744` (2026-05-03 08:52) and removed in `82915e6` (2026-05-03
14:45), the same day; that commit's message does not mention it. Every
release tag (v0.1.3 to v0.9.0) contains both commits, so no release and no
published crate carried it (#504). The bytes remain in the repository's
history. `mkfs` now writes its own two-instruction halt loop as boot code.

## Test images

Every NTFS volume committed for a test or a fuzz seed has its formatter's
boot code zeroed, keeping the BPB and the boot signatures: the 26 Windows
interrupted-write partitions (#619), and the mkntfs-made fuzz volumes, their
boot-sector seeds and the fragmented-INDX fixture (#487). CI's bootstrap
guard (`scripts/check-interrupted-bootstrap.py`) checks every tracked NTFS
image, every boot-sector seed and every data file under `src/`.

## Prose: nothing copied

On 2026-10-11, against `ee4f530`, every comment in `src/**/*.rs`, plus
`docs/**/*.md` and the README, was compared by word shingles with the text of
all 65 pages of the Linux-NTFS documentation (#489). One 10-word run and
eight 7-word runs were shared, all factual: a `$LogFile` field-table row in
`docs/spec/sections/05-logfile-journal.md`, a run of hex zeros, and "the last
two bytes of each sector". No documentation prose was copied. A comparison
with the `ntfs` crate 0.4.0 found five shared 8-word comment runs (one
sentence about the NTFS-to-Unix epoch offset) and 32 shared 30-token code
windows, all inside the attribute type-code enum (the standard type codes and
names). No logic was ported. There are no porting markers (`ported from`,
`adapted from`, `copied from`) anywhere in `src/`.

## Implementation source consulted

One, on record: while fixing `$EA_INFORMATION` (#338), its layout was
corroborated against the structure headers of two GPL implementations,
ntfs-3g and the Linux kernel's NTFS driver (#488). The layout has since been verified independently, from
Windows-authored `$EA_INFORMATION` values with zero, one and two `NEED_EA`
entries (`ea_information_windows_oracle`, #611). The source policy in
`AGENTS.md` now forbids opening copyleft implementation source at all.

Two permissively licensed implementation files have been opened since, and
neither for the format: the usage text and exit constants of Slate's
MIT-licensed `src/tools/bin/ntfs-chkdsk.rs`, to drive it as an oracle (#687),
and the sector-rescue functions of its MIT-licensed
`src/tools/recovery_io.rs`, for the rescue scan's behaviour (#720; see
Sources for both).
