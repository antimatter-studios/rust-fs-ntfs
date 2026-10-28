[Previous: Geometry](01-geometry-boot.md) | [TOC](../ntfs-specification.md) | [Next: Mapping and allocation](03-data-runs-bitmap.md)

# 2. MFT records and attributes

<a id="overview"></a>

## Overview

The Master File Table is itself a file. Record 0 describes its unnamed `$DATA`,
whose mapping pairs locate the record stream, and its unnamed `$BITMAP`, whose
bits track occupied record slots. Record 0 is bootstrapped from the boot-sector
MFT LCN. Later records must be located through `$MFT:$DATA`, including extents
added during growth. **IMPLEMENTED:** [mft_io.rs](../../../src/mft_io.rs)
`mft_record_offset_io`; [mft_bitmap.rs](../../../src/mft_bitmap.rs).

The contiguous expression `mft_lcn * cluster_size + n * record_size` applies to
the initial contiguous extent, not arbitrary record `n` after fragmentation.
The low-level implementation retains a bounded-use recovery fallback when record
0's fixup fails. Its existence is not proof that an unreadable MFT is contiguous.
The current single-offset MFT I/O refuses a record crossing a run boundary.

Microsoft's [FILE record header](https://learn.microsoft.com/en-us/windows/win32/devnotes/file-record-segment-header)
publishes selected fields for NTFS 3.0/3.1 while reserving others. The fuller
layout below records the source's interpretation of those bytes; reserved fields
in the public declaration are not independently documented by that declaration.

<a id="record-header"></a>

## FILE record header

**IMPLEMENTED:** [record_build.rs](../../../src/record_build.rs)
`write_file_record_header`, [mft_io.rs](../../../src/mft_io.rs).

| Offset | Bytes | Field | Use |
| --- | --- | --- | --- |
| `00` | 4 | Signature | `FILE` |
| `04` | 2 | USA offset | Relative to record start |
| `06` | 2 | USA word count | Update word plus saved tail words |
| `08` | 8 | LSN | Last stamped logged modification; replay comparison |
| `10` | 2 | Reuse sequence | Part of every nonzero-sequence file reference |
| `12` | 2 | Hard-link count | Link accounting; not blindly the count of all namespace aliases |
| `14` | 2 | First attribute offset | After header and USA, aligned to 8 |
| `16` | 2 | Flags | `0001` in use, `0002` directory index present, `0008` view index present; formatter pairs `0004` with `0008` on its view-index records (see below) |
| `18` | 4 | Bytes used | Includes the attribute terminator reservation |
| `1C` | 4 | Bytes allocated | Record size; a read refuses any other value (#509) |
| `20` | 8 | Base file reference | Zero in base; owner reference in extension |
| `28` | 2 | Next attribute instance | Allocator cursor for new attributes |
| `2A` | 2 | Reserved/alignment | Not interpreted as a checksum |
| `2C` | 4 | Record number | Emitted by our 3.1 builders |

The formatter reserves eight bytes for the end marker, beginning with type
`FFFFFFFF`, and includes them in `bytes_used`. Attribute walking stops at the
marker or the used-size boundary. Alignment and used-size errors have historically
produced chkdsk corrections; unused tail bytes are not another attribute.
**IMPLEMENTED / historical observation:** [attr_io.rs](../../../src/attr_io.rs),
[mkfs bug catalog](../../mkfs-bug-catalog.md).

Records hosting a view index (`$Secure`, `$ObjId`, `$Reparse`, `$Quota`) carry
flags `000D` in this formatter: in use plus `0004` and `0008`. The public header
page reserves `0004`, so its pairing comes from Windows behavior rather than
documentation. The source records that `chkdsk /f` left `$Extend` children at
`000D` after repairing them, and that chkdsk reported "Flags for file record
segment 9 are incorrect" when `$Secure` lacked the view-index bit.
**IMPLEMENTED / historical observation:** `mkfs::build_system_record_with_parent`.

<a id="usa-fixup"></a>

## Update sequence array

A protected structure replaces the final two bytes of each protection stride with
an update sequence number. The USA stores that number first, followed by the
original two-byte words. A reader compares the stored number with each tail
before restoring the words. A mismatch identifies an incomplete transfer.
**DOCUMENTED:** [MULTI_SECTOR_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/multi-sector-header).

For the source's stride `S` and buffer size `R`:

```
protected_strides = R / S
usa_words = protected_strides + 1
first_attribute = align8(usa_offset + 2 * usa_words)
```

For a 1024-byte FILE record with 512-byte strides and USA at `30`, the count is
3 words and the first attribute begins at `38`. With a 4096-byte record and the
same stride, it begins at `48`. These offsets must be derived, not copied into
all builders. **IMPLEMENTED:** `write_file_record_header`, `validate_usa_geometry`.

The read functions require the expected magic, a fitting USA, and matching tails.
The write functions save clean buffer tails, advance the update number while skipping
zero on wrap, then stamp
every tail. Mutators receive the restored buffer. Writing an already encoded
buffer as though it were clean would save update numbers as payload and destroy
the original tail bytes. FILE and INDX operations use boot-sector logical-sector
geometry; log pages use 512-byte strides. See [the geometry caveat](01-geometry-boot.md#sector-vs-cluster).
**IMPLEMENTED:** `apply_fixup_on_read_magic`, `apply_fixup_on_write_magic`.

USA protection does not detect arbitrary interior bit flips. Ordinary FILE
records in this implementation have no mandatory CRC32 footer. The earlier draft's
instruction to reserve or compute one was unsupported.

<a id="sequence-numbers"></a>

## File references and reuse

```
reference = (record_number & 0x0000FFFFFFFFFFFF) | (sequence << 48)
```

A slot number identifies storage; the sequence distinguishes successive occupants.
The writer computes a new sequence on allocation, with wrap skipping zero.
`read::resolve_path` compares a directory entry's nonzero sequence against the
record it follows and refuses a stale entry. A zero reference sequence bypasses
this comparison in the current code; that is an implementation rule, not a blanket
statement that every NTFS reference should omit sequence checks.
**IMPLEMENTED:** `record_build::encode_file_reference`,
`mft_io::next_sequence_for_slot`, `read::refuse_if_stale`;
[record_sequence_reuse.rs](../../../tests/record_sequence_reuse.rs),
[stale_index_entry.rs](../../../tests/stale_index_entry.rs).

*When* the sequence advances differs between the two writers this source models.
This driver advances it when it allocates a slot (`next_sequence_for_slot`: stored
value plus one), and frees a record by clearing the in-use flag alone
(`write::remove_file_record_io`). The replay handler for Windows'
`DeallocateFileRecordSegment` advances it at the moment of freeing ([§5](05-logfile-journal.md#opcodes)),
so a record Windows frees already carries its next sequence. If Windows keeps that
value when it reallocates, a slot freed here and then reused by Windows would keep
the old reference: a stale index entry, catalog key, or change-journal record
naming the deleted file would match the new one. **IMPLEMENTED / OPEN:** whether
Windows advances the sequence again on allocation is unestablished; freeing a file
here, creating one on Windows, and comparing both references would settle it.

The formatter gives system records `max(1, record_number)` as their sequence, so the
root is reference (5, 5), and every system file's parent reference must name it
that way. The source records that a constant 1 produced chkdsk's "Incorrect
information was detected in file record segment" on records 2–11.
**IMPLEMENTED / historical observation:** [mkfs.rs](../../../src/mkfs.rs) `build_system_record_with_parent`.

Listings read index metadata without following every child record. Consequently a
stale name may be listed and then refused when opened. The public reader does not
promise a full consistency scan on each enumeration.

<a id="system-files"></a>

## System records

The first 16 slots are reserved by this formatter. Runtime allocation starts after
them and after occupied `$Extend` children. Reserved does not mean that every slot
must contain an active file. **IMPLEMENTED:** [mkfs.rs](../../../src/mkfs.rs).

| Record | Name | Primary content or responsibility |
| --- | --- | --- |
| 0 | `$MFT` | Record stream and record allocation bitmap |
| 1 | `$MFTMirr` | Duplicate initial FILE records |
| 2 | `$LogFile` | Metadata restart log |
| 3 | `$Volume` | Label and version/flags |
| 4 | `$AttrDef` | Attribute definitions emitted by formatter |
| 5 | `.` | Root directory |
| 6 | `$Bitmap` | Cluster allocation map |
| 7 | `$Boot` | Boot stream |
| 8 | `$BadClus` | Bad-cluster reservation stream `$Bad` |
| 9 | `$Secure` | Security catalog and view indexes |
| 10 | `$UpCase` | UTF-16 uppercase table |
| 11 | `$Extend` | Directory for extended metadata |
| 12–15 | Reserved | In-use placeholders, no filename/root key |

Our formatter uses 16 for `$ObjId`, 17 for `$Reparse`, and 18 for `$Quota`.
These child numbers are a layout choice, not portable discovery keys. Find them
through `$Extend` on foreign volumes.

<a id="mft-mirror"></a>

## MFT mirror

The formatter writes fixed in-use placeholders in slots 12–15 without filename
attributes or root directory keys. Such reservations are not ordinary named files
and do not acquire a link count merely because the in-use bit is set.

The formatter mirrors four FILE records. `sync_mftmirr_record_io` copies one of
records 0–3 using the first run of `$MFTMirr:$DATA`; higher record numbers are a
no-op. The general MFT update primitive does **not** automatically mirror every
write. Volume flag/label/version paths, MFT growth, and replay call mirror sync
where their code requires it. **IMPLEMENTED:** [mft_io.rs](../../../src/mft_io.rs),
[fsck.rs](../../../src/fsck.rs), [write.rs](../../../src/write.rs);
[mftmirr_freshness.rs](../../../tests/mftmirr_freshness.rs).

Mirror size and coverage on foreign large-cluster geometries remain **OPEN**.
The constant four and first-run addressing should not be presented as a proven
universal mirror policy. Nor is there a general repair decision matrix selecting
between every semantic disagreement in primary and mirror.

<a id="attr-header"></a>

## Common attribute header

A record contains variable-length attribute records. Their type and optional name
identify the stream; their instance identifies the particular attribute within a
record. Records are aligned to eight bytes. **DOCUMENTED:**
[ATTRIBUTE_RECORD_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header).
**IMPLEMENTED:** [attr_io.rs](../../../src/attr_io.rs), [record_build.rs](../../../src/record_build.rs).

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Type code |
| `04` | 4 | Entire attribute record length |
| `08` | 1 | Form: 0 resident, 1 nonresident |
| `09` | 1 | Name length in UTF-16 units |
| `0A` | 2 | Name offset from attribute start |
| `0C` | 2 | Flags: compression bits, `4000` encrypted, `8000` sparse |
| `0E` | 2 | Instance/attribute ID |

<a id="resident-nonresident"></a>

### Resident form

| Offset | Bytes | Field |
| --- | --- | --- |
| `10` | 4 | Value length in bytes |
| `14` | 2 | Value offset from attribute start |
| `16` | 1 | Resident indexed flag in our builders |
| `17` | 1 | Reserved |

The name and value are not necessarily immediately adjacent to their header.
Respect offsets and containing lengths. `find_attribute(..., None)` means an
*unnamed* attribute, not the first attribute of any name.

### Nonresident form

| Offset | Bytes | Field |
| --- | --- | --- |
| `10` | 8 | Lowest VCN |
| `18` | 8 | Highest VCN, inclusive |
| `20` | 2 | Mapping-pairs offset |
| `22` | 2 | Compression-unit exponent interpreted by source |
| `24` | 4 | Reserved |
| `28` | 8 | Allocated length |
| `30` | 8 | Logical data length |
| `38` | 8 | Initialized length / VDL |
| `40` | 8 | Physical allocation field in sparse/compressed header variants |

The last field is not present in every ordinary 64-byte header. Names and mapping
pairs must be placed after the chosen variant. Size fields belong to the lowest-VCN
segment, not to each continuation independently. See [mapping](03-data-runs-bitmap.md).

<a id="std-info"></a>

## Standard information

The source reads both the 48-byte common form and 72-byte extended form.
**IMPLEMENTED:** `write::read_si_full_io`, `record_build::write_standard_information`.

| Value offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | Creation time |
| `08` | 8 | Content modification time |
| `10` | 8 | MFT change time |
| `18` | 8 | Access time |
| `20` | 4 | File attribute flags |
| `24` | 4 | Maximum versions |
| `28` | 4 | Version number |
| `2C` | 4 | Class ID |
| `30` | 4 | Owner ID, extended form |
| `34` | 4 | Security ID, extended form |
| `38` | 8 | Quota charged, extended form |
| `40` | 8 | USN, extended form |

Times use 100 ns intervals since 1601-01-01 UTC. `set_times` changes only standard
information. Filename and directory-key copies are not automatically rewritten.
The source's full reader returns version-specific fields only when they fit.

The formatter gives system files hidden plus system (`0006`) without archive,
matching the source's record of Microsoft format output; user files start with
archive (`0020`). View-index hosts also carry `20000000` here.
The flag setter accepts read-only `0001`, hidden `0002`, system `0004`, archive
`0020`, normal `0080`, temporary `0100`, offline `1000`, and not-content-indexed
`2000`, and refuses any other bit in either its add or remove mask (#502).
Directory `0010`, device `0040`, sparse `0200`, reparse `0400`, compressed `0800`,
and encrypted `4000` describe structure the file must actually have, so a caller
cannot set them alone. These file flags are distinct from attribute-header transform
flags and FILE record flags. NTFS filename directory/view-index bits are discussed
in [chapter 4](04-indexes-directories.md#fn-directory-bit).
**IMPLEMENTED:** `write::file_attr`, `record_build` constants.

<a id="attribute-list"></a>

## Attribute lists and extension records

An attribute list is a sequence of variable-size entries identifying an attribute,
its first VCN, and the record holding it. **IMPLEMENTED:** `read::parse_attribute_list`.

| Entry offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Type |
| `04` | 2 | Entry length |
| `06` | 1 | Name length in UTF-16 units |
| `07` | 1 | Name offset from entry start |
| `08` | 8 | Lowest VCN |
| `10` | 8 | File reference of containing record |
| `18` | 2 | Attribute instance |
| variable | variable | Optional UTF-16 name |

The current reader consults the list before returning a base-record attribute.
A single VCN-zero entry is followed to its record. When an attribute's run list is
split into several segments, each in its own record, the reader stitches them
(#473). It finds each segment by type, name, and instance, refuses a resident
segment, and decodes each segment's mapping pairs on their own: every segment
restarts its LCN deltas. It shifts them to the segment's lowest VCN, then appends
them in VCN order. The segments must tile the stream: each starts where the
previous one ended, its header's VCN range matches what its runs map, and the last
ends at the allocated size the VCN-zero segment declares. A gap, overlap, or
missing segment is refused, naming the VCN. A stream is read whole or not at all;
returning the VCN-zero segment alone would be a short read reported as a complete
one. Lengths, flags, and compression unit come from the VCN-zero segment's header.
Nonresident attribute lists must be fully mapped and initialized: zero-filling a
hole would manufacture a false terminator.
**IMPLEMENTED:** `read::locate_attribute`, `read::stitch_segments`;
[attribute_list_split.rs](../../../tests/attribute_list_split.rs),
[native_read_fixtures.rs](../../../tests/native_read_fixtures.rs).

The single-segment path still matches by type and name alone, and neither path
checks the extension record's base reference or the list entry's sequence.
List name validation also needs entry-local bounds review.
These are **OPEN** integrity gaps, not guarantees of the current implementation.
Writers that maintain a list go through `attr_list::ListedFile`, which creates
the list when a base record fills (#480). Paths that do not maintain it refuse a
listed record (`attr_resize::refuse_if_attribute_list`, #468). The design, and
what is still refused, is [attribute-list-writes.md](../../attribute-list-writes.md).

<a id="attribute-validation"></a>

## Attribute iteration and malformed records

The iterator bounds `bytes_used` by the buffer, requires lengths aligned to eight,
checks the form-specific minimum header, checks names inside the attribute, and
checks resident value or nonresident mapping-pair offsets against that attribute.
It treats any nonzero form byte as nonresident. Invalid fields commonly stop the
iterator by returning `None`, the same result used for a normal end marker.
**IMPLEMENTED:** `attr_io::AttrIter::next`.

Thus “all yielded attributes are bounded” is stronger than “a whole malformed
record always returns a parse error”: callers using `find_attribute` can report a
missing attribute when iteration actually stopped at an earlier malformed one.
Unknown numeric types can be iterated; the typed enum covers only recognized types.
A stricter fallible walk and distinguishing corruption from absence remain Q22.

<a id="attr-compaction"></a>

## Attribute resizing and IDs

`resize_resident_value` and `replace_attribute` move following attributes and the
terminator inside the fixed record, update lengths and `bytes_used`, and refuse a
record overflow. NTFS orders a record's attributes by type code and then by name,
and readers stop searching once they pass the place an attribute would sort to.
Within a type, the name decides, not the write order: if `aa` lands after `zz`
because it was written later, ntfs-3g cannot find `aa` (#467).
`insert_attribute_collated` orders a new attribute among those of its type by name:
case-insensitively under the volume's `$UpCase`, then case-sensitively, with an
unnamed attribute first. The named-stream writers use it. Other inserts use
`insert_attribute_sorted`, which honours type order only and appends after
attributes of the same type. Whether several unnamed attributes of one type, such
as a hard link's second `$FILE_NAME`, need a further order is not established.
ID allocation advances the record's next-instance
field; it does not create extra disk space. **IMPLEMENTED:**
[attr_resize.rs](../../../src/attr_resize.rs);
[attr_resize.rs tests](../../../tests/attr_resize.rs).

There is no proven universal resident-data threshold. Available room depends on
record size, name length, existing streams, indexes, and other attributes. Promotion
to nonresident storage changes the representation without creating a new file.
No general nonresident-to-resident compaction or extension collapse policy is
implemented or inferred for Windows chkdsk.

## Open questions

Q04–Q06 cover extension identity, fragmented MFT records, and mirror geometry.
Q15 covers cached filename metadata and alias/link accounting.

[Previous: Geometry](01-geometry-boot.md) | [TOC](../ntfs-specification.md) | [Next: Mapping and allocation](03-data-runs-bitmap.md)
