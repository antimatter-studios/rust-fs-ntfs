# Knowledge gaps and resolving experiments

This register records incomplete knowledge separately from missing implementation.
An **implementation gap** means the desired mechanism is known but absent or
partial. An **evidence gap** means we have not established the format or Windows
behavior sufficiently. An **integrity gap** identifies a validation boundary in
current code. None is filled by treating our formatter and reader agreeing as an
independent oracle.

The source review below is at `ee4f530628f507976ddac261906f389823b99c26`,
2026-10-11, updated to `c4b32996b52c295786deb5081a6171eeb35ca5c7` on 2026-10-12. New experiments should retain image hashes, producing Windows version,
geometry, pre/post bytes, operation sequence, and chkdsk verdicts. Reuse the normal
tier/matrix workflow and exclusive VM access; do not quietly skip missing tools.

## Geometry and records

### Q01 — USA strides on 4Kn and mixed geometry

**Evidence and probable implementation gap.** FILE/INDX calls use logical-sector
geometry; LFS uses 512-byte strides. Microsoft multisector notes discuss a 512-byte
stride, and the repository's own test-side reader, the `ntfs` crate, applies a fixed
512-byte stride to every record. `validate_usa_geometry` instead requires
`usa_count - 1 == record_size / bytes_per_sector`. On a 4Kn volume a 4096-byte
record with a 512-byte stride has a nine-word USA, which this driver would refuse
on every record. The driver and its own oracle disagree here, and no 4Kn fixture
has decided between them. Establish exact
USA count, saved-tail positions, and read/write behavior for Windows-authored 4Kn
FILE/INDX/RSTR/RCRD records, including records equal to one sector. Compare the
same bytes before and after a real Windows metadata mutation. A synthetic record
using our own stride proves only internal consistency.

### Q02 — Boot-copy location and arbitration

**Evidence and implementation gaps.** The formatter writes the backup at the final
sector of the formatted extent, excluding it from its sector-count field. General fallback/repair
is absent. Capture Windows formats inside partitions and raw volumes, document the
relationship among device length, partition length, `$Boot` length, and declared
sector count. Corrupt primary only, backup only, and contradictory-valid geometry;
record Windows mount and chkdsk decisions without assuming “copy the valid one” is
always the policy. Include resized partitions and truncated image exports.

### Q03 — Historical format and size limits

**Evidence gap.** Parser bounds are not universal Windows limits. Compare 1.2,
3.0, and 3.1 images for record header variants, SI sizes, attribute alignment, boot
encodings, and index geometry. Obtain independent version-specific evidence before
claiming maximum file/volume/index sizes. Verify large-cluster sectors-per-cluster
bytes around `80`, `F8`, and `F4`; do not repeat the old `>= 80` exponent rule.
The writable mount's version-field update does not establish a complete upgrade.

### Q04 — Attribute-list identity and segment validation

**Implementation and integrity gaps.** Single VCN-zero extension lookup works,
and a run list split into segments is stitched or refused, never read short.
Stitching matches each segment by instance; the single-segment path does not,
and neither checks the extension's base reference or the entry's sequence.
Create Windows files with many ADS and fragmented multi-record runs. Corrupt one
sequence, instance, base reference, name offset, and entry length at a time.
Require either complete correctly assembled data or explicit refusal, never a
successful VCN-zero-only prefix. Check entry-local name bounds, minimum entry size,
ordering, duplicate/overlapping segments, and sparse/uninitialized list rejection.

Tracking: #468, #471, #472; stitching landed with #473.

### Q05 — MFT records crossing extent boundaries

**Implementation gap.** Record addressing follows `$MFT:$DATA`, but its current
single-range helper refuses a record spanning runs. Find or construct independently
graded fragmented MFT fixtures whose FILE record crosses an extent boundary at
small clusters. A future piecewise implementation must preserve USA over the whole
record, reject holes and device overruns, and ensure writes touch no neighboring
file. Also validate continuation segments of `$MFT`'s own mapping list.

### Q06 — Mirror coverage and maintenance

**Evidence and implementation gaps.** The formatter and sync helper use four
records and first-run mirror mapping. Capture Windows mirror sizes across record
and cluster geometries, including clusters larger than four records. Establish
coverage from stream size rather than folklore. Mutate each mirrored record,
compare both copies, then test damaged primary/mirror combinations. Separate
structural readability from semantic disagreement. Review every caller modifying
records 0–3 for required mirror sync; generic RMW does not supply it automatically.

Tracking: #481.

## Allocation and indexes

### Q07 — Mapping-pair reference ambiguities

**Evidence gap.** Microsoft's developer page describes signed VCN increments and
mentions unallocated LCN zero; our decoder accumulates unsigned lengths while the encoder emits positive signed
widths, and both use absence of an LCN field for holes. Use Windows-generated run lists with lengths near sign-bit
boundaries, negative deltas, first-run LCN zero if obtainable, holes between allocated
runs, and explicit/truncated terminators. Compare retrieval pointers and raw bytes.
Pin the count-width encoding and whether accepted unterminated lists are compatible,
without interpreting a documentation ambiguity as permission to guess.

### Q08 — Complete ownership and structural checks

**Implementation and integrity gaps.** The allocator protects known metafile
ranges but is not a global ownership validator. Fsck does not reconstruct all
ownership. Design independent corruption cases for two files sharing clusters,
allocated-but-unreachable records, missing bits, leaked clusters, duplicate keys,
and extension-record owners. A scan must prove completeness before clearing any
allocation bit. If a system metafile cannot be located, the protection helper's
best-effort omission may weaken protection; test that case explicitly. No safe
TRIM/discard or general repair policy follows from the present limited scan.

Tracking: #515, #517.

### Q09 — Index VCN units when clusters exceed blocks

**Evidence and implementation gaps.** Bitmap enumeration converts block ordinals
with `ordinal * block_size / cluster_size`; new-block allocation uses rounded
cluster counts. On subcluster blocks these formulas need reconciliation and can
collapse different ordinals. Format Windows volumes with 64 KiB–2 MiB clusters
and several INDX blocks. Compare block header VCNs, child pointers, bitmap ordinals,
root geometry bytes, and physical offsets. Establish whether sector/subcluster
units apply, then require lookup, enumeration, insertion, and free-block reuse to
use the same independently established conversion.

### Q10 — Case-sensitive-directory metadata and upcase versions

**Evidence and implementation gaps.** Ordinal comparison exists but directory
lookup/insertion currently use upcase collation. Determine the actual Windows/WSL
per-directory policy storage via control queries and raw pre/post images; do not
invent a FILE_NAME or SI flag position. Test `foo`/`FOO`, Unicode code units,
unpaired surrogates, and reopening under Windows. Compare upcase captures across
Windows versions before describing one embedded table as universal.

### Q11 — Tree mutation beyond current growth limits

**Implementation gap.** Existing node splits/root promotion work within resident
bitmap/mapping capacity. Nonresident bitmap growth, interior-key deletion, merges,
root collapse, and variable-length rename in overflowed parents remain bounded or
unsupported. Grow Windows-created directories through multiple levels, split
interior nodes, delete routing keys, and remove every child. Require correct
Windows enumeration as well as chkdsk. Test block reuse, resident bitmap exhaustion,
nonresident bitmap transition, and failure at every publishing step. A 256-file
insertion oracle is not evidence for all these cases.

Tracking: #474, #495.

## Recovery and duplicated metadata

### Q12 — Additional LFS layouts and restart arbitration

**Evidence and implementation gaps.** Replay is checked for supported LFS 2.x
captures. Capture active older-version logs, alternate table widths, equal-LSN
conflicting restart copies, wrap generations, and unsupported opcodes. Establish
tail/home selection and redo/undo targets from Windows recovery before enabling a
new handler. Prepared/committed but not forgotten checkpoint transactions need
independent captures and correct durable state transitions.

### Q13 — A second crash during replay or a driver mutation

**Implementation and evidence gaps.** Planning writes nothing, but applying a plan
and resetting its log involves multiple device writes. Runtime mutations do not
emit their own WAL. Inject failure/crash after each write and sync, restart on
Windows from the exact resulting image, and compare recovery. Establish whether
replay is safely restartable for every supported operation, especially sequence
bumps, resident resize, index insertion, and partial log reset. Do not describe
preflight as transaction atomicity or assume callback sync means stable media.

### Q14 — USN stream lifecycle

**Implementation and evidence gaps.** V2 consumer fields are documented, but the
driver does not implement `$J`/$Max persistence, reason coalescing, trimming,
journal identifiers, or V3/V4 readers. Capture Windows creation, writes, rename,
link changes, sparse wrap, and deletion/recreation, and correlate returned API
records with stream bytes. Keep metadata recovery evidence separate from change
notification history.

Tracking: #479.

### Q15 — Filename caches, aliases, and link counts

**Evidence and compatibility gaps.** SI/stream metadata and FN/index copies can
diverge. Path-based writes now refresh the sizes in each name's index entry, but
times and the record's own filename copies are left unchanged. Hard-link paths assume their
supported namespace relationships. Capture Windows after content changes, timestamp
set, chmod-style flag changes, case-only rename, short-alias creation/removal, and
multiple hard links in one or several parents. Compare all filename attributes,
index keys, link count, and authoritative stat/stream queries. Determine refresh
rules and alias accounting rather than requiring every copy to match at all times.

Tracking: #493; index-entry sizes landed with #506, case-differing paths with #491.

## Payloads and extended metadata

### Q16 — Compressed-unit topology and resource limits

**Evidence and integrity gaps.** The LZNT1 codec bounds individual chunks; unit
mapping groups allocated clusters and holes. Validate alternating/misplaced holes,
short output, trailing tokens, clipped final units, unusual unit exponents, and
initialized-size boundaries against Windows-authored volumes. Current compressed
range reads materialize the whole stream; a streaming/unit-window path and fixed
memory policy remain implementation work. Codec self-tests cannot prove malformed
mapping topology is handled identically to Windows.

### Q17 — Reparse forms and transformed content

**Evidence and implementation gaps.** Symlink/mount-point print extraction and
resident raw writes exist. Nonresident raw reparse access, GUID-form third-party
envelopes, substitute-name following/cycles, LX symlink payloads, AppExecLink,
cloud placeholders, WOF, and dedup content require their own parser/semantic work.
Retain Windows-produced examples and API results for each tag/form. Reading a
print name is not proof of resolving a target. Recognizing an encrypted bit does
not decrypt EFS content.

Tracking: Windows-syntax symlink targets landed with #505.

### Q18 — Strict EA parsing and summaries

**Integrity and implementation gaps.** Resident packing is implemented; the reader
does not validate every NUL/alignment/overlap/trailer/summary invariant. The
[Windows summary oracle](../../../tests/ea_information_windows_oracle.rs) covers
differing compact/query sizes and zero, one or two NEED_EA flags; its generated
Windows fixtures must pass CI before that evidence is accepted. Mutate the
name terminator, entry-local length, next offset, flags, and summary independently.
Require explicit malformed-input outcomes; determine which trailing padding is
valid. Nonresident EA storage and Windows byte-name comparison beyond ASCII require
additional examples. Do not claim a 65535-byte value fits a resident FILE record.

Tracking: #482.

### Q19 — Security catalog scaling and ACL behavior

**Evidence and implementation gaps.** Small SDS catalog serialization and an
existing security-ID setter are implemented. General entry packing across 256 KiB
boundaries, duplicate reuse/collisions, mirror groups, runtime SDH/SII insertions,
owner/DACL inheritance, and access enforcement are not. Compare Windows catalogs
across mirror boundaries and duplicate descriptors, corrupt one index/header/hash,
and correlate actual file security queries. Match descriptor bytes as well as hashes.
Audit valid/null/absent ACLs and SID bounds before exposing a full ACL API.

Tracking: the security-ID existence check landed with #503.

### Q20 — Global object and reparse catalogs

**Implementation gap.** Local setters do not maintain `$ObjId:$O` or `$Reparse:$R`.
Capture object/reparse create, overwrite, rename, unlink, and hard-link operations.
Pin key/value structures, ordering, uniqueness, and update lifetime. Verify chkdsk
with both local attributes and global catalog keys, not just payload round-trip.
Track this separately from the already implemented setters.

Tracking: #478.

### Q21 — Quotas, utility streams, and modern features

**Evidence and implementation gaps.** Formatter quota defaults and one captured
quota replay opcode are not accounting/enforcement. Quota-owner indexes, threshold
semantics, EFS metadata/crypto, TxF logged utility structures, and modern placeholder
systems remain outside the implemented model. Acquire primary format/API sources
and Windows fixtures per feature; leave layouts unnamed where evidence is absent.
The presence of `$Extend` children or type `100` is not a working subsystem.

Tracking: #510.

### Q22 — Attribute iteration and diagnostic completeness

**Integrity and implementation gap.** `AttrIter` bounds yielded attributes but
returns `None` for malformed headers as well as the normal terminator. Design
foreign-volume corruption cases where a broken early attribute precedes valid
unnamed data, a volume label, or an ADS. Record which callers report corruption,
absence, or a partial list. A future fallible iterator should preserve the format
error without breaking valid unknown types or empty values. Include non-0/1 form
bytes, name/header overlap, sparse-header variant size, and missing end markers.

## Resolved or withdrawn older claims

| Earlier statement | Current disposition |
| --- | --- |
| NTFS requires exactly 512-byte logical sectors | Withdrawn. Source parses other power-of-two sector sizes; 4Kn details are Q01. |
| `80` sectors-per-cluster byte uses negative exponent form | Corrected. `80` is literal 128; only values above it use the parser's exponent form. |
| Ordinary NTFS 3.1 FILE records require a CRC32 footer | Withdrawn. No basis in source or checked header documentation; USA is the implemented protection. |
| Log replay absent / undo mostly disabled | Corrected. Supported LFS 2 redo and active-transaction undo are implemented; Q12/Q13 bound them. |
| LZNT1 reads absent | Corrected. Native decompression exists; compression writing remains absent. |
| No MFT growth or directory splitting | Corrected. Both exist with the explicit storage/capacity limits in §3/§4. |
| Nonresident `$BITMAP:$I30` cannot be read | Corrected. It can be read; growing it remains a limitation. |
| Native reads and upcase loading use the upstream crate | Corrected. Production uses native primitives; upstream remains a test oracle. |
| `TXBG`/`TXCM` are NTFS transaction log signatures | Withdrawn. Supported logs use LFS headers, types, and client opcodes. |
| Every FN attribute always contributes one independent hard link | Withdrawn as a universal rule. DOS alias pairs require independent accounting evidence. |
| Clear dirty flag proves safe to discard the log | Withdrawn. Restart state is inspected regardless of dirty flag. |
| Old 42/46 scenario count proves current full validation | Replaced by revision-specific retained 72-scenario evidence with its scan-fallback/platform limits. |

The older speculative repair recipes are not normative NTFS rules. Git history
preserves that draft; this register supplies concrete ways to recover useful
knowledge without carrying unsupported instructions into an implementer's reference.
