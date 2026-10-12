# NTFS storage and behavior specification

## Reading guide

NTFS represents a file as a collection of attributes in one or more Master File
Table (MFT) records. A pathname is a route through directory indexes to a record;
it is not the file's identity. A stream can occupy bytes inside the record or
clusters elsewhere on the volume. The record describes those clusters with a
mapping-pairs list. Allocation bitmaps describe ownership at different levels.
Metadata recovery adds another layer: a record on disk may be older than a logged
change that must be replayed before writing safely.

These relationships are implemented in [record_build.rs](../../src/record_build.rs),
[read.rs](../../src/read.rs), [data_runs.rs](../../src/data_runs.rs),
[index_io.rs](../../src/index_io.rs), and [logfile_replay.rs](../../src/logfile_replay.rs).
Microsoft's [MFT developer notes](https://learn.microsoft.com/en-us/windows/win32/devnotes/master-file-table)
describe the record-and-attribute model. **DOCUMENTED / IMPLEMENTED.**

Read chapters 1–4 to follow a pathname down to physical bytes, chapter 6 for the
payload structures attached to files, and chapter 5 for interrupted operations.
Chapter 7 joins them into file lifecycles and explains the current driver's limits.

## Contents

| Chapter | Topic | Main questions |
| --- | --- | --- |
| [1](sections/01-geometry-boot.md) | Geometry and boot | Where does addressing start? What are the units and size encodings? |
| [2](sections/02-mft-records.md) | Records and attributes | How are records protected, referenced, reused, and extended? |
| [3](sections/03-data-runs-bitmap.md) | Mapping and allocation | Where are stream bytes? What do holes and initialized length mean? |
| [4](sections/04-indexes-directories.md) | Names and indexes | How do directory keys route to records and grow into trees? |
| [5](sections/05-logfile-journal.md) | Recovery and change journal | How do restart pages, redo, undo, and change notifications differ? |
| [6](sections/06-special-streams.md) | Other payloads and system files | How are EAs, reparse data, security, volume metadata, and IDs represented? |
| [7](sections/07-file-lifecycle.md) | Behavior across structures | What changes during create, write, resize, link, rename, delete, and mount? |

Supporting material:

- [Evidence conventions](readme.md)
- [Source and test coverage](coverage.md)
- [Knowledge gaps and experiments](notes/open-questions.md)
- [Primary references](notes/references.md)

## Conventions

All numeric fields in the tables are little-endian unless stated otherwise.
Offsets are hexadecimal bytes relative to the named containing structure.
`u16`, `u32`, and `u64` indicate unsigned integers of that width; `i8` and signed
mapping-pair deltas require sign extension. A UTF-16 length is a count of 16-bit
code units unless the field explicitly counts bytes. Names are not NUL-terminated
unless their structure says so.

- **LCN**: logical cluster number relative to the volume's beginning.
- **VCN**: virtual cluster number relative to an attribute's logical stream.
- **File reference**: 48-bit record number plus 16-bit reuse sequence.
- **USA**: update sequence array, protecting multi-stride transfers.
- **LSN**: log sequence number in `$LogFile`.
- **USN**: change-journal sequence number, or update sequence number in a USA;
  context distinguishes these unrelated uses of the abbreviation.

A sector, cluster, FILE record, INDX block, and log page are different units.
Never substitute one size for another. Nor should a bitmap bit be called a VCN:
its meaning depends on which bitmap is being read.

Limits in parser code describe what this driver accepts. Format limits and Windows
version limits require independent evidence. In particular, NTFS does not require
512-byte logical sectors exclusively, all files do not fit in one MFT record, and
the MFT is not an indefinitely contiguous array after its bootstrap extent.

## Boundaries of this reference

This is a source-grounded reference, not a claim to have reconstructed all NTFS.
Kernel caching, file sharing and open-handle semantics, ACL enforcement, VSS,
BitLocker, Storage Spaces, EFS cryptography, and TxF recovery remain outside the
implemented storage model. Structures known only by name are included as gaps,
not filled with invented field layouts.

The previous draft's proposed CRC32 footer on ordinary NTFS 3.1 FILE records,
`TXBG`/`TXCM` log signatures, and disabled-undo recovery model have no basis in the
reviewed implementation or linked record documentation. They are removed rather
than left as instructions to a future implementer. USA checks detect incomplete
transfers; they are not general checksums of record contents.

## Revision record

| Date | Change |
| --- | --- |
| 2026-05-03 | Initial six-chapter draft. |
| 2026-05-25 | Earlier source-derived updates to security hashes, collation and namespaces. |
| 2026-10-11 | Reconciled chapters with native read/write, MFT growth, index splitting, compression reads, and redo/undo replay; separated evidence types; added lifecycle explanations and a concrete question register. |
| 2026-10-12 | Corrected the rename-ordering, attribute-ordering and `$AttrDef` flag descriptions; recorded the sequence-number timing and 4Kn stride divergences, system-record sequences and flags, and the formatter's volume flags; linked open questions to their issues; brought the chapters up to date with the fixes merged through `c4b3299` (split run lists, collated stream and rename ordering, index-entry sizes, metafile refusal, write-time mount preparation, symlink syntax, security-ID and header checks). |
