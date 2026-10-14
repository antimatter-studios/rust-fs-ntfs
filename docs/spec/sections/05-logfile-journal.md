[Previous: Indexes](04-indexes-directories.md) | [TOC](../ntfs-specification.md) | [Next: Special payloads](06-special-streams.md)

# 5. Recovery log and change journal

<a id="overview"></a>

## Two different journals

`$LogFile` supplies metadata restart records used to recover interrupted changes.
`$Extend\$UsnJrnl` reports changes to consumers such as indexers; it is not a
replacement for redo and undo data. The USA's update sequence number is another
unrelated use of “USN.” **IMPLEMENTED:** [logfile.rs](../../../src/logfile.rs),
[logfile_replay.rs](../../../src/logfile_replay.rs). **DOCUMENTED:**
[Change journals](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journals).

Most internal LFS fields below are **IMPLEMENTED**, with behavioral evidence from
Windows captures rather than a complete public Microsoft disk-format specification.
The source names its historical Windows Internals input; that attribution alone
is not a retained independent experiment.

<a id="sizing"></a>

## Log storage and versions

Record 2's unnamed `$DATA` holds the log. Its restart pages declare system page
size, log page size, stream size, record offsets, and LFS version. Log-page size,
cluster size, and FILE record size can differ. Current replay accepts LFS major 2
and rejects older replay layouts it has not validated. The clean-state reader can
recognize supported clean LFS 1.x and 2.x shapes without replaying them.
**IMPLEMENTED:** `logfile_replay::plan`, `logfile::state`.

The current fsck replay path locates a contiguous log range and materializes the
whole log. Fragmented log support and a streaming memory budget are distinct
unimplemented improvements; neither is implied by fragmented INDX support.
**IMPLEMENTED:** `fsck::locate_logfile_data_io`, `replay_logfile_io`.

<a id="restart-area"></a>

## Restart page pair

Restart pages begin at log offset zero and the declared system-page size. Readers
validate each using `RSTR` magic and USA at a 512-byte stride, then choose the
valid page with the greater `current_lsn`. One invalid copy can be tolerated; two
invalid copies cannot establish a trusted state. This is the code's selection
policy; cross-version tie and wrap arbitration remain Q12.
**IMPLEMENTED:** `logfile::restart_page`, `logfile_replay::restart`.

<a id="restart-page"></a>

### Restart page prefix

| Page offset | Bytes | Field used or emitted by source |
| --- | --- | --- |
| `00` | 4 | `RSTR` |
| `04` | 2 | USA offset |
| `06` | 2 | USA word count |
| `08` | 8 | Checkdisk/LSN-related header field; not used as current LSN |
| `10` | 4 | System-page size |
| `14` | 4 | Log-page size |
| `18` | 2 | Restart-area offset |
| `1A` | 2 | LFS minor version |
| `1C` | 2 | LFS major version |

<a id="restart-area-struct"></a>

### Restart area

Offsets below are relative to the restart area selected by page offset `18`.

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | Current LSN |
| `08` | 2 | Client count |
| `0A` | 2 | Free-client list head |
| `0C` | 2 | In-use-client list head; `FFFF` means none |
| `0E` | 2 | Flags; `0002` means clean dismount |
| `10` | 4 | Sequence-number bits in LSN geometry |
| `14` | 2 | Restart-area length in emitted layout |
| `16` | 2 | Client-array offset |
| `18` | 8 | Log-file size |
| `20` | 4 | Last-LSN data-length field in emitted layout |
| `24` | 2 | Record header length |
| `26` | 2 | Record-page data offset |

The clean flag here is not the dirty flag in `$VOLUME_INFORMATION`.

<a id="client-record"></a>

### Client record

The source uses `A0`-byte client slots. It reads the in-use client's restart LSN
at client offset `08`; the client array is relative to the restart area.
A restart LSN names an NTFS checkpoint record, not an arbitrary first redo record.
Unrecognized client/table shapes are refusal cases, not guessed layouts.

<a id="rcrd-page"></a>

## Record pages and LSN addressing

| Page offset | Bytes | Meaning in source |
| --- | --- | --- |
| `00` | 4 | `RCRD` |
| `04` | 2 | USA offset |
| `06` | 2 | USA word count |
| `08` | 8 | Last-LSN or home-offset field depending on layout/copy |
| `10` | 4 | Page flags |
| `14` | 2 | Page count |
| `16` | 2 | Page position |
| `18` | 2 | Next record offset |
| `20` | 8 | Last LSN ending on page |
| declared data offset | variable | Record content |

The source uses `last_end_lsn`, rather than always treating `08` as an LSN. In a
clean LFS 1.x tail copy the latter can be its home byte offset. Inferring the
newest record from the wrong field can mistake an old checkpoint for pending work.
**IMPLEMENTED / Windows fixture check:** `logfile.rs`;
[windows_interrupted_fixture.rs](../../../tests/windows_interrupted_fixture.rs).

<a id="lsn"></a>

### Log sequence number

An LSN combines wrap sequence with an offset in eight-byte units. The low offset
bits resolve to bytes using the restart area's `seq_number_bits`. The code uses
wrapping/bit operations equivalent to keeping the offset portion and shifting it
left three. Its geometry checks reject implausible page sizes and bit counts.
Do not store an LSN as a plain byte offset or drop its wrap generation.
**IMPLEMENTED:** `logfile_replay::Pages`, LSN helpers in [logfile.rs](../../../src/logfile.rs).

LFS 2.x writes recent pages into a tail-copy area before their home. Replay selects
valid newer copies, handles records continuing across pages, and accounts for a
record wrapping from the last page to the first normal record page. A tail copy's
ending LSN can refer to a record beginning on the prior page. Equal-newness copies
with incompatible bytes are refused. **IMPLEMENTED:** `Pages`; regression/oracle
cases in `windows_interrupted_fixture.rs`.

<a id="log-record-header"></a>

## LFS record and NTFS client data

The supported replay layout uses a `30`-byte LFS record header:

| Record offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | This LSN |
| `08` | 8 | Client previous LSN |
| `10` | 8 | Client undo-next LSN |
| `18` | 4 | Client-data length |
| `1C` | 4 | Client identifier |
| `20` | 4 | Record type: 1 client operation, 2 client restart |
| `24` | 4 | Transaction ID |
| `28` | 2 | Record flags |
| `2A` | 6 | Reserved/alignment |

The client operation begins with:

| Client offset | Bytes | Field |
| --- | --- | --- |
| `00` | 2 | Redo opcode |
| `02` | 2 | Undo opcode |
| `04` | 2 | Redo data offset |
| `06` | 2 | Redo data length |
| `08` | 2 | Undo data offset |
| `0A` | 2 | Undo data length |
| `0C` | 2 | Open-attribute table target |
| `0E` | 2 | LCN count |
| `10` | 2 | Record offset |
| `12` | 2 | Attribute offset |
| `14` | 2 | Cluster block offset |
| `16` | 2 | Reserved |
| `18` | 8 | Target VCN |
| `20` | `8 * count` | Target LCN array |

Offsets belong to their stated header/target, not the volume. Restart client data
has its own checkpoint/table interpretation. **IMPLEMENTED:** replay record walking
and dispatch in [logfile_replay.rs](../../../src/logfile_replay.rs).

For flagged (`0004`) records the supported source may extend a short redo payload
with zeros to the declared length. Windows captures show resident data recovering
as zeros in these cases. This is a particular log encoding/behavior; it is not
LZNT1 decompression and does not license padding arbitrary truncated log records.

<a id="opcodes"></a>

## Opcodes and current dispatch

Numbers below are taken from the source's dispatcher. “Control” means analysis or
transaction handling, not a disk mutation applied like a filename update.

| Code | Name | Current treatment |
| --- | --- | --- |
| `00` | Noop | Control/no mutation |
| `01` | Compensation | Undo-chain/control semantics |
| `02` | InitializeFileRecordSegment | FILE mutation |
| `03` | DeallocateFileRecordSegment | Clear in-use and bump nonzero sequence |
| `04` | WriteEndOfFileRecordSegment | Refused mutation |
| `05` / `06` | Create/DeleteAttribute | FILE mutation |
| `07` | UpdateResidentValue | FILE mutation including supported resize |
| `08` | UpdateNonresidentValue | Raw mapped data |
| `09` | UpdateMappingPairs | FILE attribute resize and highest-VCN recomputation |
| `0A` | DeleteDirtyClusters | Refused mutation |
| `0B` | SetNewAttributeSizes | FILE mutation |
| `0C` / `0D` | Add/DeleteIndexEntryRoot | Root mutation |
| `0E` / `0F` | Add/DeleteIndexEntryAllocation | INDX mutation |
| `10` | WriteEndOfIndexBuffer | INDX mutation |
| `11` / `12` | SetIndexEntryVcnRoot/Allocation | Child VCN in entry tail |
| `13` / `14` | UpdateFileNameRoot/Allocation | Filename-key mutation |
| `15` / `16` | Set/ClearBitsInNonresidentBitmap | Mapped bitmap mutation |
| `17` | HotFix | Refused mutation |
| `18` | EndTopLevelAction | Control |
| `19` / `1A` / `1B` | Prepare/Commit/ForgetTransaction | Transaction analysis; unresolved prepared/committed states refused |
| `1C` | OpenNonresidentAttribute | Open-table analysis |
| `1D`–`20` | Attribute/table dumps | Checkpoint analysis as supported |
| `21` | UpdateRecordDataRoot | Supported view-index data mutation |
| `22` | UpdateRecordDataAllocation | Supported view-index data mutation in an INDX block; the block LSN is left as Windows leaves it |
| `23` / `24` | UpdateRelativeDataInIndex / 2 | Refused mutation |
| `25` | ZeroEndOfFileRecord | FILE mutation |

An opcode name is not enough evidence to implement it. Unsupported operations in
a required replay cause refusal of the whole plan before it writes to the device.
The driver does not silently skip an unknown mutation and declare recovery complete.

<a id="tracking-tables"></a>

## Checkpoint and restart tables

The supported NTFS checkpoint body is at least `40` bytes and has version 1.
Fields below are those actually consumed by the planner, not invented names for
reserved bytes. **IMPLEMENTED:** `logfile_replay::plan`.

| Body offset | Bytes | Consumed field |
| --- | --- | --- |
| `00` | 4 | Checkpoint format version |
| `08` | 8 | Analysis start LSN; zero starts at this checkpoint |
| `10` | 8 | Open attribute table dump LSN |
| `20` | 8 | Dirty page table dump LSN |
| `28` | 8 | Transaction table dump LSN |

A zero dump LSN means no table of that kind. Otherwise the referenced operation
must be the expected dump opcode. Its redo-data offset/length locates table bytes.
A restart table begins with `u16 entry_size` at `00` and `u16 entry_count` at `02`;
entry slots begin at table offset `18`. The first `u32` of an allocated entry is
`FFFFFFFF`. The slot's byte offset is also an identity: open-attribute targets and
checkpoint transaction IDs use it, rather than simply numbering occupied entries.

| Entry kind | Offsets consumed by source |
| --- | --- |
| Open attribute (`28` bytes) | Type `u32` at `08`; file reference `u64` at `10` |
| Dirty page (declared variable size) | Target `u32` at `04`; LCN count `u32` at `0C`; first VCN `u64` at `10`; oldest LSN `u64` at `18`; LCN array at `20` |
| Transaction (at least `28` bytes) | State byte at `04`, active = 1; last LSN `u64` at `10`; undo-next LSN `u64` at `18` |

A dirty-page array can contain zero for a not-yet-mapped cluster. The planner
records cluster-relative entries so a mutation in any part of a logged page can
find the remaining cluster locations. Nonzero recorded and checkpoint LCNs must
agree. A FILE record's target is checked against the MFT after applying planned
mapping changes, since the log itself can grow that MFT.

<a id="wal-recovery"></a>

## Analysis, redo, and undo

The plan reconstructs the checkpoint's open attribute table, dirty page table,
and transaction table, then updates them with subsequent records. Supported open
attribute and transaction table entries have size `28`; other layouts are not
assumed compatible. Dirty pages identify an oldest LSN and cluster locations.
On 1 KiB clusters, a 4 KiB logged page spans several clusters; targets can begin
partway through it. **IMPLEMENTED:** `logfile_replay::plan`.

Redo starts from the required oldest dirty-page point. A mutation is applied only
when its dirty-page state requires it and, for valid FILE/INDX blocks, the block
LSN is older. `UpdateRecordDataRoot` has a fixture-established exception: quota
view-index data can be updated without advancing that FILE record's LSN.

Undo follows every active transaction's undo-next chain newest first, including
transactions already active at the checkpoint. It applies undo payloads after
redo without using page-LSN filtering to discard work already written home.
Missing, forward-moving, or unsupported chains are refused. Prepared/committed but
not forgotten checkpoint states remain unvalidated and are refused.

The earlier draft's “undo largely disabled” model is removed: current recovery
explicitly undoes active transactions. See the Windows recovered-image comparisons
in [windows_interrupted_fixture.rs](../../../tests/windows_interrupted_fixture.rs).
These tests name the `.recovered` images produced by Windows from matching captures.

## Preflight versus durable application

`logfile_replay::plan` writes nothing; it returns modified blocks in memory.
Before applying them, fsck verifies that logged FILE targets agree with the *replayed*
MFT mapping and that writes do not overwrite `$LogFile` itself. It then writes the
plan, synchronizes mirrored records and storage, and resets the log only after
those writes. **IMPLEMENTED:** `fsck::replay_logfile_io`.

Complete preflight protects against unsupported or contradictory input. It does
not make applying many device writes atomic: an I/O failure or another crash during
application can leave a partial result. The source does not emit a new restart log
or compensation record for each of its own replay writes. This durable-restart
question remains Q13.

<a id="empty-valid-shape"></a>

## Clean-state determination

`LogfileState` distinguishes:

- `Empty`: all bytes are `FF`.
- `Clean`: no client open, restart clean flag, or a verified final empty checkpoint.
- `Pending`: work may remain or a trusted clean determination cannot be made.

Restart `current_lsn == client_restart_lsn` is insufficient by itself for LFS 2.x:
record pages may contain newer data while the restart area is unchanged. The state
reader checks those pages, including tails, and verifies the checkpoint's active
transaction and dirty-page tables. A torn page is not assumed irrelevant.
**IMPLEMENTED:** [logfile.rs](../../../src/logfile.rs).

### Formatter and reset shapes

`mkfs::logfile_head` transforms an embedded historical 12 KiB capture into the
clean-dismount profile currently written: paired LFS 1.1 restart pages marked
clean, tail copies/home checkpoint placement, and `FF` in the remaining space.
The embedded capture's original LFS 2.0 pages are therefore not the formatter's
present output. `reset_logfile_io` checks protected ranges before its fill operation;
the explicit low-level reset discards records and requires the caller to establish consistency independently. Normal fsck and writable-mount paths inspect/replay pending work first.
**IMPLEMENTED:** [mkfs.rs](../../../src/mkfs.rs), [fsck.rs](../../../src/fsck.rs).

## Dirty bit and mount policy

A clear `$Volume` dirty bit does not prove a clean restart log. The source records
Windows 8+ interrupted cases whose work is visible in the log alone. The mount
preparation path rejects a set dirty flag, rejects an unreadable log, and replays a
supported pending log before allowing writes. Explicit fsck can replay then clear
the flag; it is not a whole-volume structural checker. **IMPLEMENTED:**
`prepare_rw_mount_io`, `fsck_io`; [chapter 7](07-file-lifecycle.md#mount-and-recovery).

<a id="usn-jrnl"></a>

## USN change journal

The conventional `$UsnJrnl` has `$J` records and `$Max` configuration data. This
repository does not implement general journal emission, trimming, or a complete
on-disk journal reader. The formatter does not need to fabricate user change
history. Windows API record documentation describes consumer-visible records;
it should not be mistaken for a full sparse-stream storage/reclamation algorithm.
**DOCUMENTED / OPEN:** [Change journals](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journals), Q14.

<a id="usn-record-v2"></a>

### USN_RECORD_V2

The consumer-visible fixed portion is described by Microsoft's
[USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2).
Offsets are bytes from the record start. **DOCUMENTED**, not an implemented parser.

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Record length |
| `04` / `06` | 2 each | Major/minor version |
| `08` | 8 | File reference |
| `10` | 8 | Parent reference |
| `18` | 8 | USN |
| `20` | 8 | Timestamp |
| `28` | 4 | Reason flags |
| `2C` | 4 | Source information |
| `30` | 4 | Security ID |
| `34` | 4 | File attributes |
| `38` / `3A` | 2 each | Filename length in bytes / filename offset |
| filename offset | variable | UTF-16 filename |

Advance by `RecordLength`, not by a fixed C struct size. Honor runtime filename
offsets and version changes. V3/V4 records are different layouts; no V2 cast should
be prescribed for them. The notification history is incomplete after wrap or journal
replacement and cannot serve as a complete ownership inventory.

## Open questions

Q12–Q14 cover unsupported LFS versions, interruption during replay application,
and the unimplemented USN stream lifecycle. Q08 covers the structural checks fsck
currently does not perform.

[Previous: Indexes](04-indexes-directories.md) | [TOC](../ntfs-specification.md) | [Next: Special payloads](06-special-streams.md)
