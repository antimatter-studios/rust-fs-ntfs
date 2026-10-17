[Previous: Recovery](05-logfile-journal.md) | [TOC](../ntfs-specification.md) | [Next: File lifecycle](07-file-lifecycle.md)

# 6. Payloads, security, and extended metadata

<a id="attr-type-map"></a>

## Attribute type map

The type belongs to the attribute header; the optional name selects an instance
of that type. A stream name such as `$I30` does not change the type code.
**IMPLEMENTED:** [attr_io.rs](../../../src/attr_io.rs), [mkfs.rs](../../../src/mkfs.rs).
Microsoft lists selected types in its
[attribute header notes](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header).

| Code | Attribute | Payload or purpose |
| --- | --- | --- |
| `10` | `$STANDARD_INFORMATION` | Shared metadata, timestamps, flags, security ID |
| `20` | `$ATTRIBUTE_LIST` | Record locations of attributes/segments |
| `30` | `$FILE_NAME` | Parent relationship and name |
| `40` | `$OBJECT_ID` | Object ID and optional birth IDs |
| `50` | `$SECURITY_DESCRIPTOR` | Per-record security descriptor |
| `60` | `$VOLUME_NAME` | UTF-16 label |
| `70` | `$VOLUME_INFORMATION` | Version and flags |
| `80` | `$DATA` | Unnamed content or named stream |
| `90` | `$INDEX_ROOT` | Resident index root |
| `A0` | `$INDEX_ALLOCATION` | External index nodes |
| `B0` | `$BITMAP` | Record/index allocation map by owner |
| `C0` | `$REPARSE_POINT` | Tagged reparse payload |
| `D0` | `$EA_INFORMATION` | Extended-attribute summary |
| `E0` | `$EA` | Extended-attribute entries |
| `100` | `$LOGGED_UTILITY_STREAM` | EFS/TxF-related stream names; opaque here |

The numeric iterator/diagnostics can encounter `100`, but `AttrType` has no
`LoggedUtilityStream` variant. Recognizing its name is not equivalent to exposing
a parser or cryptographic implementation.

<a id="ads"></a>

## Alternate data streams

A file can have several `$DATA` attributes distinguished by name. The default
stream has an empty name. Windows names a stream as `file:stream:$DATA`;
the library passes the file path and stream name separately. Streams have their
own logical, allocated, and initialized lengths and transform state.
**DOCUMENTED:** [File streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams).
**IMPLEMENTED:** `read_attribute_value`, `write_named_stream_io`, `delete_named_stream_io`.

The writer first attempts a resident named value and can use nonresident storage
when the contents do not fit. Attribute name, padding, and mapping-pair header room
still consume MFT space. More streams can exhaust base-record capacity even when
cluster space is plentiful. There is no automatic overflow-record emission.
Directory ADS coexist with `$I30` metadata and are not directory entries.
**IMPLEMENTED:** [write.rs](../../../src/write.rs),
[write_ads_promote.rs](../../../tests/write_ads_promote.rs),
[ads_comprehensive.rs](../../../tests/ads_comprehensive.rs).

A stream name passes the same checks a filename does: no `\ / : * ? " < > |`,
no character below `20`, and no trailing space or period, since Windows strips
those when it parses a path. A name like `x:$DATA` or `a:b` is refused because
Windows could never address it (#497). Deletion takes any name, so a stream
written under a bad name earlier can still be removed. Windows compares stream
names case-insensitively; this writer still creates two streams for names that
differ only in case (#494). New streams are placed in name order among the
record's `$DATA` attributes ([chapter 2](02-mft-records.md#attr-compaction)).

A rewrite refused for lack of record space or clusters leaves the existing stream
as it was. A nonresident rewrite reads the old runs first and frees them only after
the new attribute is written, so the old clusters cannot be handed to the new
contents (#469). Deleting a nonresident named stream removes its attribute before
releasing its clusters. Listings report the streams in the base record only (#472).
No guarantee that all named streams are resident should be inferred from small
examples. **IMPLEMENTED:** `write::validate_win32_name`, `write_named_stream_io`,
`delete_named_stream_io`.

<a id="lznt1"></a>

## LZNT1 compression

LZNT1 read support is implemented. LZNT1 encoding and mutation of compressed stream
content are not. The current data write/grow/truncate guards reject compressed and
encrypted attribute-header flags rather than writing plaintext into transformed
storage. **IMPLEMENTED:** [compression.rs](../../../src/compression.rs),
`read::read_compressed_nonresident`, `write::refuse_transformed_data`.

<a id="lznt1-cu"></a>

### Compression units

The nonresident header's compression-unit exponent at `22` is interpreted as
`unit_clusters = 1 << exponent`. The reader accepts nonzero exponents through 4;
4 means 16 clusters. It groups logical runs by unit:

- No mapped clusters: all-zero output.
- Every unit cluster mapped: content stored uncompressed for that unit.
- Fewer clusters mapped: concatenate their physical bytes and decompress as LZNT1.

The reader bounds the unit's contribution by logical and initialized length.
A sparse suffix in compressed mappings describes unused physical allocation in a
unit; it must not simply be exposed as a zero suffix of the logical plaintext.
The source's grouping does not establish full validation of every malformed
allocated/hole arrangement. **OPEN:** Q16.

<a id="lznt1-chunk-header"></a>

### Chunk header

Each chunk starts with a little-endian `u16`:

| Bits | Meaning |
| --- | --- |
| 0–11 | Body length minus one |
| 12–14 | Signature `011` |
| 15 | Compressed token body if set; raw body otherwise |

Header zero ends the stream. A chunk can produce at most 4096 bytes. The decoder
rejects invalid signatures, truncated bodies, invalid backward displacements, and
chunk output overflow. It stops when the requested output limit is reached.
**IMPLEMENTED:** `compression::decompress_unit`.

<a id="lznt1-groups"></a>

### Token groups

A compressed body consists of a flag byte followed by up to eight tokens, with
flags consumed least-significant bit first. Zero is one literal byte; one is a
16-bit back-reference. Starting with a 12-bit length field, the decoder moves
bits into the displacement field as the current chunk output grows beyond 16,
32, 64, and subsequent position boundaries.

```
length = (token & length_mask) + 3
displacement = (token >> displacement_shift) + 1
```

References are relative to bytes already output in the current chunk. Overlap is
allowed, so a displacement of one can repeat a byte many times. A bulk nonoverlap
copy would decode it incorrectly. Each new chunk resets its output-position basis.
**IMPLEMENTED:** `split_token`, `decompress_chunk`.

A Windows-generated compressed fixture is read against known plaintext in
[native_read_fixtures.rs](../../../tests/native_read_fixtures.rs). That check is
independent of our writer, which cannot produce compressed files. Its presence is
not a claim that the missing fixture ran in this documentation session.

### Other compression systems

WOF reparse-tagged content is not ordinary LZNT1. The reader explicitly refuses the
unnamed data of WOF files, including a relevant reparse attribute located through
an attribute list, instead of returning plausible zeros from their placeholder
stream. Metadata and supported ADS remain separately accessible. Deduplication,
cloud hydration, and EFS likewise require semantics beyond raw cluster reads.
**IMPLEMENTED:** `read::refuse_wof_compressed`;
[wof_read.rs](../../../tests/wof_read.rs). **OPEN:** Q17.

<a id="ea"></a>

## Extended attributes

EA names are byte strings in the library, separate from UTF-16 stream names; values
are arbitrary bytes. The current API supports resident `$EA` and uses ASCII
case-insensitive matching for upsert/removal. Nonresident EA reads are refused.
**IMPLEMENTED:** [ea_io.rs](../../../src/ea_io.rs), `write::commit_eas`.
The query-entry envelope is independently documented in
[MS-FSCC FileFullEaInformation](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/0eb94f48-6aac-41df-a878-79f4dcfd8989);
its API alignment rules do not by themselves prove all on-disk summary semantics.

### Entry format

Offsets below are relative to one entry in the encoded `$EA` value:

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Aligned entry length, including the final on-disk entry |
| `04` | 1 | Flags; `80` = NEED_EA |
| `05` | 1 | Name length in bytes |
| `06` | 2 | Value length in bytes |
| `08` | name length | Name bytes |
| after name | 1 | Name NUL |
| after NUL | value length | Value bytes |
| end | padding | Entry rounded to four-byte boundary by encoder |

The encoder limits names to 254 bytes and values to 65535 bytes; record capacity
can impose a much smaller practical limit. Names do not acquire a UTF-16 unit count
just because they live on an NTFS volume.

<a id="ea-info"></a>

### Summary

The resident eight-byte `$EA_INFORMATION` value is:

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 2 | Compact packed length |
| `02` | 2 | NEED_EA entry count |
| `04` | 4 | Query-buffer/unpacked length |

Compact length sums `4 + name_length + 1 + value_length` per entry: it excludes
the four-byte next-offset field and alignment padding. Query length describes
the encoded query entries and is not identical to compact length.
The writer rebuilds both attributes and removes both when the list becomes empty.
**IMPLEMENTED:** `packed_ea_length`, `build_ea_information_value`, `commit_eas`.

[The Windows summary oracle](../../../tests/ea_information_windows_oracle.rs)
reads raw summary bytes from files created with `NtSetEaFile`: compact length
19, query length 32, and zero, one or two NEED_EA flags. The fixture generator
runs on Windows in CI; the image is generated on demand. This comparison
checks the on-disk summary separately from the documented API entry buffer.
It also compares the complete `$EA` bytes: Windows stores the aligned length
in the final entry's offset field too, where the API input buffer uses zero.
Writing that zero into `$EA` made chkdsk report corrupt extended attribute
information in the attribute-list EA scenario (#745).

The decoder checks value and next-offset buffer bounds, but does not implement
all conceivable NUL, alignment, overlap, trailing-partial-entry, flag, and summary
consistency checks. Do not turn proposed validation rules into claims about it.
**OPEN:** Q18; regression checks in [field_exhaustion_ea.rs](../../../tests/field_exhaustion_ea.rs).

<a id="reparse-attr"></a>

## Reparse points

Microsoft publishes the common, symlink, and mount-point envelope in
[REPARSE_DATA_BUFFER](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/content/ntifs/ns-ntifs-_reparse_data_buffer).
Third-party owners use a different
[GUID-form envelope](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_reparse_guid_data_buffer).
These documented distinctions should be kept when accepting generic payloads.

The resident writer builds this common prefix:

| Value offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Reparse tag |
| `04` | 2 | Tag-data length, excluding this prefix |
| `06` | 2 | Reserved |
| `08` | declared length | Tag-specific data |

Writing sets the standard-information reparse flag; removing clears it. The raw
reparse helper returns a tag and payload and is resident-only. Generic attribute
reading has a broader storage path; this does not remove the raw helper's limit.
**IMPLEMENTED:** `record_build::build_resident_reparse_point_attribute`,
`write::write_reparse_point_io`, `read_reparse_point_io`.

A tag declares which interpreter should handle the bytes. Arbitrary tags do not
all use the symbolic-link layout or get link traversal. Third-party GUID-form
reparse envelopes and tag-specific limits require further specification.
**OPEN:** Q17.

<a id="reparse-tags"></a>

### Recognized tag constants

| Tag | Source name | Content semantics implemented? |
| --- | --- | --- |
| `A0000003` | Mount point | Print-name extraction |
| `A000000C` | Symlink | Builder and print-name extraction |
| `80000017` | WOF | Unnamed-data refusal |
| `A000001D` | LX symlink | Constant, not native Linux-link decoding |
| `8000001B` | AppExecLink | Constant, not execution semantics |

**IMPLEMENTED:** `record_build::reparse_tag`, `lib::reparse_link_target`.
Neither a reparse bit nor an arbitrary tag implies that `readlink` should succeed.

<a id="reparse-payload-symlink"></a>

### Symbolic-link payload

Offsets are relative to tag-specific data at common-prefix offset `08`:

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 2 | Substitute name offset from PathBuffer |
| `02` | 2 | Substitute name byte length |
| `04` | 2 | Print name offset from PathBuffer |
| `06` | 2 | Print name byte length |
| `08` | 4 | Flags; 1 relative, 0 absolute in builder |
| `0C` | variable | PathBuffer, UTF-16LE |

The builder concatenates substitute and print names, uses explicit lengths, and
does not require NUL between them. It writes the target in Windows syntax, chosen
by the target's form: `\??\X:\…` for a drive path, `\??\UNC\…` for a UNC path, an
NT path as given, and anything else as a relative link with `\` separators and
flag 1. A POSIX absolute target such as `/d/small` names no drive, so it is
stored as the rooted relative path `\d\small`, resolved from the root of the link's
own volume. Reading turns `\` back into `/` in a relative target; an absolute
target keeps its Windows form. A relative link this builder writes carries the
same flags and names as one Windows wrote in the symlink fixture (#505).
**IMPLEMENTED / Windows fixture check:** `write::create_symlink_io`,
`lib::reparse_link_target`; [readlink_windows_oracle.rs](../../../tests/readlink_windows_oracle.rs).

<a id="reparse-payload-mount"></a>

### Mount-point payload

Mount points have the same first eight bytes of offsets/lengths but no flags word;
PathBuffer begins at `08`. The API extracts the print name, strips a leading
`\??\`, converts to UTF-8, and requires enough caller space for all bytes plus NUL.
It refuses a too-small buffer without returning a silently truncated target.
Disk UTF-16 conversion is lossy for unpaired surrogates; it is not byte-preserving
round-trip of all possible NTFS names. **IMPLEMENTED:** [lib.rs](../../../src/lib.rs)
`decode_symlink_print_name`, `decode_mount_point_print_name`, `fs_ntfs_readlink`;
[readlink_windows_oracle.rs](../../../tests/readlink_windows_oracle.rs).

No automatic symlink-following/cycle-resolution engine is promised by path lookup.
Reading a link's displayed name is distinct from following its substitute name.
Runtime reparse writes do not maintain `$Extend\$Reparse:$R`.

<a id="secure-file"></a>

## Security catalog

Record 9 contains `$Secure`. The formatter emits `$SDS` descriptor storage and
view indexes `$SII` by security ID and `$SDH` by hash plus security ID.
A file's extended standard information can reference the catalog through
`security_id`. That numeric ID is not a Windows SID byte string.
**IMPLEMENTED:** [sds.rs](../../../src/sds.rs), [mkfs.rs](../../../src/mkfs.rs).

<a id="sds-layout"></a>

### SDS entry

| Stream-relative entry offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Descriptor hash |
| `04` | 4 | Security ID |
| `08` | 8 | Primary entry offset in `$SDS` |
| `10` | 4 | Unpadded total size: header + descriptor |
| `14` | variable | Self-relative security descriptor |
| end | padding | Align next entry to 16 bytes |

For the formatter's single descriptor, `$SDS` uses two physical clusters with a
sparse run between logical VCN 1 and `40000 / cluster_size`. The primary and mirror
are physically adjacent allocations while they are 256 KiB apart in the stream.
Logical length stops at the mirror's unpadded entry end; stream allocation spans
the logical gap while `total_allocated` counts only the two physical clusters.
This is an example where logical adjacency and physical adjacency differ.
**IMPLEMENTED:** `mkfs::format_filesystem`, `build_sparse_nonresident_data_attribute`.

The builder copies each emitted entry verbatim at primary offset plus `40000`
(256 KiB). A mirror copy keeps the primary offset in its header. It emits a small
catalog; it does not implement arbitrary multi-block catalog packing, entry reuse,
or boundary rollover. Those need explicit rules before scaling the helper.
**IMPLEMENTED:** `sds::build_sds`. **OPEN:** Q19.

The hash iterates descriptor bytes as little-endian 32-bit words:

```
h = rotate_left(h, 3) + word   (modulo 2^32)
```

A final partial word is zero-padded by this implementation. The source records
agreement with twelve entries sampled from Microsoft-format output; this historical
claim is in `sds.rs`, while unit vectors check the implementation's calculations.
Do not confuse a descriptor hash with collision-proof identity: bytes still matter.

### View-index payloads

`$SII` has a four-byte security-ID key; `$SDH` has an eight-byte hash/ID key.
Both store the SDS header as the value. The view-entry header uses data offset and
length instead of a file reference. Data begins immediately after the key; only
the entire entry is padded to eight bytes. Aligning a four-byte key's value upward
independently changes `$SII`'s layout. **IMPLEMENTED:** `mkfs::build_view_index_entry`;
historical chkdsk evidence documented in its comment.

<a id="sds-sd-body"></a>

### Self-relative descriptor

Microsoft's [MS-DTYP security descriptor](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-dtyp/)
is the relevant public type family. The emitted descriptor starts with:

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 1 | Revision |
| `01` | 1 | Reserved |
| `02` | 2 | Control bits |
| `04` | 4 | Owner SID offset |
| `08` | 4 | Group SID offset |
| `0C` | 4 | SACL offset |
| `10` | 4 | DACL offset |

Offsets are relative to the descriptor, not SDS or volume. An ACL has revision,
size, ACE count, and variable ACE records; a SID has revision, subauthority count,
a six-byte identifier authority, and 32-bit subauthorities. The identifier authority
is not a little-endian integer like the subauthority array. The fixed formatter
blob is not a general ACL parser or permission evaluator.
**IMPLEMENTED:** `mkfs::SD_SYSFILE_RW`, `sds::SdEntry`.

The formatter supplies a canonical catalog entry at ID `100`. Its system records
reference that entry through the security ID in 72-byte standard information;
`build_system_record` does not emit a per-record `$SECURITY_DESCRIPTOR` attribute.
Runtime records also use 72-byte standard information, initially zeroed extended
fields; callers can point a file at an existing catalog ID. `set_security_id`
reads every `$SII` key, in the root and each allocated index block, and refuses an
ID none of them holds before writing anything (#503). It does not insert a new SDS
entry. ACL inheritance, access enforcement, descriptor insertion/deduplication,
and index growth are gaps.
**IMPLEMENTED / OPEN:** `write::set_security_id_io`, Q19.

<a id="upcase"></a>

## Upcase table

Record 10's unnamed data is a 131072-byte array of 65536 little-endian `u16`
mappings. Lookup uses each UTF-16 code unit as an index. The formatter embeds a
Windows-captured table; the reader loads the volume's table through native attribute
I/O, requires at least the table length, and uses the first 131072 bytes.
**IMPLEMENTED:** [upcase.rs](../../../src/upcase.rs).

The source records that generating this table with modern Rust Unicode casing
changed 327 BMP entries relative to its capture. Its embedded bytes have SHA-256
`41c26bc7a12bdaeb26025c93118697c7e3ef81ee048b00fe5cce2a472e0e0742`.
That describes this captured profile, not a claim that every historical Windows
version used exactly those bytes. Failure to load the table is not silently
converted into ordinal path lookup. **OPEN:** Q10.

<a id="volume-file"></a>

## Volume metadata

<a id="volume-name"></a>

### Label

`$VOLUME_NAME` is UTF-16LE without a required trailing NUL. An absent attribute
reads as an empty label; corruption reading it remains an error. The setter
validates and resizes the resident attribute and synchronizes record 3's mirror.
**IMPLEMENTED:** `read::read_volume_info`, `write::set_volume_label_io`;
[volume_label.rs](../../../tests/volume_label.rs).

<a id="volume-information"></a>

### Volume information

| Value offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | Reserved |
| `08` | 1 | NTFS major version |
| `09` | 1 | NTFS minor version |
| `0A` | 2 | Flags; low bit `0001` is dirty in source |

The formatter emits version 3.1 and flags `0080`: dirty clear, one other bit set.
The source records that a fresh Windows format carries `0080`, and that with flags
`0000` `ntfs.sys` queued a proactive scan on every mount and `chkdsk /scan` exited 13
on otherwise clean metadata. It deliberately leaves upgrade-on-mount (`0004`) clear:
a `/scan` snapshot mount is read-only, so the upgrade cannot complete there.
**IMPLEMENTED / historical observation:** `mkfs::format_filesystem` and the
volume-information notes in [fsck.rs](../../../src/fsck.rs).

The name of `0080` is disputed. The source calls it `MODIFIED_BY_CHKDSK`, citing
Tuxera's public documentation. The upstream `ntfs` crate gives that name to `8000`.
Treat it as the bit a fresh Windows format sets, not as a statement that chkdsk ran.
**OPEN.**

Dirty clearing/setting and a narrow version-upgrade helper operate here, not in the
boot sector. A set dirty bit means the volume may be inconsistent, and Windows
runs chkdsk on it at the next restart. **DOCUMENTED:**
[fsutil dirty](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-dirty).
Clearing the bit cancels that check (#519). It does not reconstruct lost indexes or
allocation state. **IMPLEMENTED:** [fsck.rs](../../../src/fsck.rs).

<a id="attrdef"></a>

## Attribute definitions

`$AttrDef:$DATA` describes supported attribute types in the formatter profile.
The source emits fifteen 160-byte entries and one zero entry:

| Entry offset | Bytes | Field |
| --- | --- | --- |
| `00` | 128 | Name, up to 64 UTF-16 code units |
| `80` | 4 | Type code |
| `84` | 4 | Display rule |
| `88` | 4 | Collation rule |
| `8C` | 4 | Definition flags |
| `90` | 8 | Minimum size |
| `98` | 8 | Maximum size; all ones for the emitted unbounded convention |

The table uses definition flags `40` (must be resident), `02` (indexable), and
`80`. These are not attribute transform bits. The source names `80` `NONRES`, but
that cannot be its meaning. `$DATA` lacks it and is routinely nonresident.
`$BITMAP` and `$REPARSE_POINT` carry it and are commonly resident; this driver
writes reparse points resident. `80` is usually described as "always log": changes
to the attribute are logged even when it is nonresident. No Microsoft page documents
these bits. **OPEN** for the name. The source records the table as one `chkdsk /scan`
accepts, and notes a Windows reference carrying `80` on `$BITMAP`. Table
constraints do not substitute for checking actual available record space.
**IMPLEMENTED:** `mkfs::build_attrdef_table` (whose old comment incorrectly called
the name area 64 *bytes* rather than 64 UTF-16 units).

<a id="extend-directory"></a>

## Extend metadata

The formatter links `$ObjId`, `$Reparse`, and `$Quota` under `$Extend` with view-index
record/filename flags and indexes. It uses its own compact record assignments;
Windows-formatted images must be discovered by name. The runtime does not maintain
all these indexes merely because it can write corresponding attributes.
**IMPLEMENTED:** [mkfs.rs](../../../src/mkfs.rs), [write.rs](../../../src/write.rs).

<a id="objid"></a>

### Object IDs

`$OBJECT_ID` can hold a 16-byte object ID alone, or 64 bytes with birth volume,
birth object, and domain IDs at offsets `10`, `20`, and `30`. The API reads and
writes these forms and supports idempotent removal. IDs are stored bytewise; blindly
printing them as four little-endian integers is not a UUID serialization rule.
**IMPLEMENTED:** `build_resident_object_id_attribute_full`,
`write_object_id_extended_io`; [set_object_id_extended_h.rs](../../../tests/set_object_id_extended_h.rs).

The formatter creates `$ObjId:$O`; runtime object-ID writes do not update the
volume-wide object-ID index, enforce uniqueness across files, or maintain link
tracking. **OPEN:** Q20.

<a id="reparse-index"></a>

### Reparse index

`$Reparse:$R` is the view index for reparse identities, conventionally keyed by
tag and file reference. The formatter emits the empty root; runtime reparse writes
change the file attribute and standard-information flag without inserting/removing
the catalog key. Attribute support and catalog consistency are separate features.
**IMPLEMENTED / OPEN:** Q20.

<a id="quota"></a>

### Quota indexes

The formatter emits `$Q` collated by owner ID and `$O` collated by SID. For its
sub-4096-byte cluster profile it supplies default owner ID 1 in `$Q`; at larger
clusters it leaves that index empty for Windows initialization. Source comments
record different first-mount/chkdsk behavior behind that choice; it is not a
universal Windows quota algorithm.

The emitted 48-byte quota control value is:

| Offset | Bytes | Field / emitted default |
| --- | --- | --- |
| `00` | 4 | Version 2 |
| `04` | 4 | Flags 1 |
| `08` | 8 | Bytes used, zero |
| `10` | 8 | Change time, zero |
| `18` | 8 | Warning threshold, -1 |
| `20` | 8 | Hard threshold, -1 |
| `28` | 8 | Exceeded time, zero |

**IMPLEMENTED:** `mkfs::build_default_quota_control`. Replay supports captured
`UpdateRecordDataRoot` changes to quota data, but runtime allocation does not
implement quota accounting/enforcement. A recoverable captured quota update does
not establish a complete quota subsystem.

### Formatter definition constraints

The actual emitted minimum/maximum pairs, in bytes, are:

| Type | Minimum | Maximum |
| --- | --- | --- |
| Standard information | 48 | 72 |
| Attribute list | 0 | -1 convention |
| Filename | 68 | 578 |
| Object ID | 0 | 256 |
| Security descriptor | 0 | -1 convention |
| Volume name | 2 | 256 |
| Volume information | 12 | 12 |
| Data | 0 | -1 convention |
| Index root / allocation / bitmap | 0 | -1 convention |
| Reparse point | 0 | 16384 |
| EA information | 8 | 8 |
| EA / logged utility stream | 0 | 65536 |

**IMPLEMENTED:** `mkfs::build_attrdef_table`. These are emitted definition entries,
not a promise the typed APIs can create every value they permit. For example,
object-ID APIs expose 16/64-byte forms, and resident reparse/EAs are limited by
FILE-record space. The table's zero/size conventions describe this format profile,
not all historical `$AttrDef` tables.

<a id="logged-utility-stream"></a>

## Logged utility streams and unknown payloads

Microsoft identifies `$EFS` and `$TXF_DATA` as logged utility stream names in
[File streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams).
This driver neither decrypts EFS file data nor implements TxF transactions. An
unknown attribute may remain unchanged during an unrelated in-record mutation,
but preservation by copying a record is not semantic validation of its contents.
The typed attribute enum does not expose all utility-stream operations.

No speculative EFS header/crypto layout, cloud placeholder hydration recipe,
deduplication interpretation, or TxF recovery contract is supplied. These need
primary types and independent fixtures before this reference can describe them
reliably. **OPEN:** Q17, Q21.

[Previous: Recovery](05-logfile-journal.md) | [TOC](../ntfs-specification.md) | [Next: File lifecycle](07-file-lifecycle.md)
