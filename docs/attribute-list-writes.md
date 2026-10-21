# Writes that outgrow an MFT record

Design note for #480: should a write that no longer fits its file's base
MFT record grow into extension records behind an `$ATTRIBUTE_LIST`, or keep
being refused? What the crate does today is on the
[features page](features.md); this note records the decision, the method,
the limits and how each step is graded.

## The question

When a file's attributes no longer fit its base record, NTFS moves some of
them into extension records and records where each one went in an
`$ATTRIBUTE_LIST`. Windows and ntfs-3g both do this. This crate's writer did
not, and refused instead:

```
write_ea_io(.., 2000-byte value)          -> "replace_attribute: growing by 2016 exceeds record capacity (…)"
write_named_stream_io(.., 3rd/4th stream) -> "no room for new attribute: need 1496 more, have 280"
```

Each refusal wrote nothing, so it was a correct fail-closed outcome. But
ordinary workloads reach that ceiling: a file with several EAs and streams,
many hard links, or a long run list on a fragmented volume. The operation
fuzzer met it in 17 of its first 20 sequences.

## Decision: grow into extension records, as Windows does

The writer creates and maintains an `$ATTRIBUTE_LIST`. It does not raise
the ceiling by any other means. The reasons:

- **Refusing does not scale.** The ceiling is a 1 KiB record. Callers meet it
  in ordinary use, not only in pathological cases.
- **Promotion is not an alternative.** `$FILE_NAME` must be resident, so a
  file with many long names can only grow through a list (see W2.5 in
  [future-features.md](future-features.md)).
- **The format is already read.** The readers follow lists (see below), so
  a list this crate writes is read back by the same code that reads the ones
  Windows writes.
- **chkdsk grades it.** Windows' own layout is the reference, and every step
  can be checked against Windows `chkdsk` in the matrix.

## What the read side already does

Chapter 2 of the spec, [Attribute lists and extension
records](spec/sections/02-mft-records.md#attribute-list), is the reference.
In short:

- The list is consulted before the base record, and a VCN-0 entry is followed
  to its record (`read::locate_attribute`).
- A run list split across several records is stitched in VCN order (#473).
  A gap, an overlap or a missing segment is refused, naming the VCN, so a
  stream is read whole or not at all (`read::stitch_segments`,
  `tests/attribute_list_split.rs`, checked against the independent `ntfs`
  crate).
- A non-resident list must be fully mapped and initialised.
- The spec lists open integrity gaps. The single-segment path matches by
  type and name alone. Neither path checks an extension record's base
  reference or a list entry's sequence. Validating every list is #667.

## The method, measured on Windows

`src/attr_list.rs` documents what Windows did to a file given 96 small named
streams, one at a time:

- The base record keeps `$STANDARD_INFORMATION` and the list. The first
  time the base record fills, every other attribute moves to one new
  extension record. After that, new attributes go back into the base record.
- Each later fill moves a batch of attributes out to a new extension record.
- Once the list itself no longer fits, it goes non-resident.
- An extension record has a link count of 0 and the flag IN_USE alone. Its
  `base_file_record_reference` names the base record and its sequence, and
  its attribute ids count from 0.
- The list has one entry per attribute and none for itself. Entries are
  ordered by type, then name, then starting VCN.

The writer follows the same method, with one deliberate difference: to make
room, it moves only named `$DATA` streams. `$FILE_NAME` and the other
attributes stay in the base record unless they no longer fit there. Windows'
layout allows either choice, and chkdsk grades both.

Every change is planned in memory first, so a change refused for any reason
writes nothing. The writes then go in an order that keeps the list from
ever naming something that is not on disk:

1. the list's clusters;
2. the extension records;
3. the base record that holds or maps the list.

Records and clusters the list no longer names are freed last.

## The split-run limitation

Reading an attribute split across records works (#473). Writing one is
harder: every extent has to be replaced, and each one has to stay listed
with its starting VCN. A path that does not do that refuses the attribute
rather than move one segment of it (`ListedFile::find`: "is split across N
records, and this driver does not move a segment of one"). Four issues cover
it:

- #664: a run list too long for one record is split into extents.
- #800: rewriting, writing into or truncating an unnamed `$DATA` that is
  already split.
- #805: rewriting a named stream that is already split.
- #1058: rewriting a split `$EA`.

## The slices

The work lands one behaviour at a time, each in its own issue and each
graded on its own:

| Issue | Behaviour |
|---|---|
| #610 (#480) | Named streams on a full base record create the list |
| #660 | EAs go through the list |
| #661 | An EA set too large for a resident `$EA` is stored non-resident |
| #662 | Hard links, unlink and rename go through the list |
| #663 | Unnamed `$DATA` writes go through the list |
| #664 | A run list too long for one record is split into extents |
| #665 | Reparse points and object ids go through the list |
| #666 | A directory whose base record is full gets a list, and index edits keep it true |
| #667 | The checker validates every list |

Each slice is meant to narrow the Refused row on the features page, which
names #480, as it lands. That row has not kept up with the stack: #1166.

## What stays refused

The writer still refuses some cases, each with nothing written:

- a list that is compressed or encrypted;
- a list that no longer fits, in a base record holding nothing this driver
  moves;
- a split attribute on a path that does not replace every extent;
- an attribute added to or removed from a listed record by a path that does
  not go through the list. The remaining callers of
  `attr_resize::refuse_if_attribute_list` are these: inserting an attribute
  in place, retiring clusters into `$BadClus`, and editing a directory whose
  `$INDEX_ROOT` is not in its base record.

## How each slice is validated

Our own reader cannot prove that what we wrote is right, so every slice is
graded against oracles that are not this crate:

- **Windows `chkdsk`, in the matrix.** Each slice adds two kinds of scenario.
  In the first, this crate grows a list on a volume it formatted and Windows
  grades it. In the second, Windows writes the list and this crate writes
  through it. For example:
  - named streams: `mac-format-mac-write-streams-win-chkdsk`,
    `mac-format-mac-write-big-streams-win-chkdsk`,
    `win-format-win-write-streams-mac-write-streams-win-chkdsk`;
  - EAs: `mac-format-mac-write-streams-mac-write-eas-win-chkdsk`,
    `win-format-win-write-streams-mac-write-eas-win-chkdsk`.

  A volume passes only if chkdsk reports no errors and every stream reads
  back.
- **An independent reader.** Shapes that this crate's own writer cannot
  produce, such as a split run list, are built by hand and read through the
  `ntfs` crate as well as ours (`tests/attribute_list_split.rs`). ntfs-3g
  reads volumes Windows spread over extension records
  (`tests/ntfs3g_populated.rs`).
- **Refusals.** A refused write must leave the image byte-for-byte unchanged
  (`tests/attribute_list_records_refuse_changes.rs`).
- **The checker.** Once #667 lands, `check_io` reports a list entry whose
  target record lacks the attribute, an extension attribute missing from the
  list, an entry that names the list itself, and entries out of type order.
  This lets the operation-sequence harness catch a bad list without a VM.

Linux's ntfs3 driver can confirm that what we wrote reads back. It does not
grade integrity (see [AGENTS.md](../AGENTS.md)), so it is not the oracle for
this work.
