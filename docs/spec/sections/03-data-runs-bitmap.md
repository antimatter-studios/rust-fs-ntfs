[Previous: MFT records](02-mft-records.md) | [TOC](../ntfs-specification.md) | [Next: Indexes](04-indexes-directories.md)

# 3. Data runs and allocation

<a id="overview"></a>

## Logical streams and physical storage

A nonresident stream is logically ordered by VCN even when its clusters are
physically fragmented. A mapping-pairs list says where each logical run lives.
The stream header separately describes logical size, initialized size, and
allocation. Microsoft documents per-stream sizing in
[File streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams).
**DOCUMENTED / IMPLEMENTED:** [data_runs.rs](../../../src/data_runs.rs),
[read.rs](../../../src/read.rs).

<a id="data-run-encoding"></a>

## Mapping-pair encoding

Each run begins with one descriptor byte:

```
low nibble  = number of bytes encoding cluster count
high nibble = number of bytes encoding relative LCN
```

The count follows first, then the signed LCN delta. A zero descriptor terminates
the list. A zero high nibble denotes a sparse hole: there is no LCN field. Sparse
runs advance logical VCN without updating the previous allocated LCN.
**IMPLEMENTED:** `decode_runs`, `encode_runs`.

The decoder uses 1–8 count bytes as an unsigned integer and 0–8 delta bytes as a
sign-extended integer. It refuses zero run lengths, truncated fields, negative
absolute LCNs, and overflow in accumulated LCN or VCN. It accepts exhaustion of a
bounded attribute buffer without an explicit zero terminator. Acceptance is not
proof that all Windows writers omit the terminator.

The encoder chooses minimal *positive signed* count width and minimal signed delta
width. Counts with the top bit set acquire a leading zero byte; it refuses counts
above the positive `i64` range even though the decoder accumulates a `u64`.
It requires VCN-contiguous runs beginning at zero. A continuation segment in an
extension record restarts its LCN deltas from zero and maps from its own lowest
VCN; the reader decodes it alone and shifts it into place
([attribute lists](02-mft-records.md#attribute-list)). The encoder does not write
continuation segments.
Microsoft's [attribute header notes](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header)
describe mapping pairs, but differ in wording about count sign and unallocated LCN.
The executable representation here uses a missing LCN field for holes. **OPEN:**
Q07 records this reference ambiguity rather than treating inconsistent wording as
a license to reinterpret a real run list.

<a id="data-run-examples"></a>

## Worked bytes

These examples specify the source codec's interpretation. **IMPLEMENTED:**
[data_runs.rs](../../../src/data_runs.rs); [data_runs tests](../../../tests/data_runs.rs).

| Bytes | Decoding |
| --- | --- |
| `11 03 20 00` | 3 clusters at LCN 32; VCNs 0–2 |
| `01 05 00` | 5 sparse clusters; VCNs 0–4 |
| `11 03 20 11 02 F0 00` | 3 clusters at LCN 32, then 2 at LCN 16 because `F0` is -16 |
| `11 03 20 01 02 11 01 10 00` | 3 at LCN 32, 2 holes, 1 at LCN 48; sparse run preserves previous LCN 32 |
| `21 01 80 00 00` | 1 cluster at LCN 128; signed delta needs two bytes to avoid -128 |
| `12 80 00 20 00` | Encoder form for 128 clusters at LCN 32; count uses two bytes so its high sign bit is clear |

The previous LCN is the starting LCN of the prior allocated run, not its end.
Using the end would shift every later extent. Padding after the terminator is not
another run.

<a id="vcn-lcn-mapping"></a>

## VCN-to-LCN translation

For a run beginning at VCN `V`, length `N`, and allocated LCN `L`:

```
V <= requested_vcn < V + N
physical_lcn = L + (requested_vcn - V)
byte_offset = physical_lcn * cluster_size + offset_inside_cluster
```

A hole has no physical offset. `vcn_to_lcn` returns `None` for a hole or unmapped
VCN. Callers must decide whether that means zeros or a corrupt structural stream.
The implementation searches runs; binary-search caching is not an NTFS format
requirement. `cluster_span` separately checks arithmetic and device extent.
**IMPLEMENTED:** `data_runs::vcn_to_lcn`, `mft_io::cluster_span`.

<a id="run-invariants"></a>

## Three stream lengths

- **Logical length** controls EOF and how many bytes readers return.
- **Initialized length** limits which logical bytes contain valid content.
- **Allocated length / physical allocation fields** describe reserved or physically
  occupied storage according to the normal versus sparse/compressed header variant.

Do not assume physical allocation equals rounded-up logical length for sparse or
compressed streams. Allocation can also exceed EOF. The ordinary reader returns
zeros past initialized length up to EOF, and zeros for sparse holes. A request
starting at EOF returns no bytes. A request crossing EOF returns only the remainder.
**IMPLEMENTED:** `read_attribute_range`, `read_value_from_record`.

The uncompressed ranged reader loads only clusters overlapping the window.
Resident and compressed reads currently use a whole-value fallback then slice.
A bounded length prevents absurd values relative to device size but is not a fixed
memory budget; a large legitimate whole-value request can still allocate heavily.

For parsed structures such as `$ATTRIBUTE_LIST`, missing runs or short initialized
length are errors. Returning manufactured zeros would look like an early end of a
valid table. This is a semantic distinction between file content and metadata,
not a reason to forbid ordinary sparse file data.

<a id="sparse-runs"></a>

## Sparse files

Sparse holes read as zeros, as described by Microsoft's
[fsutil sparse documentation](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-sparse).
The driver can replace a file with sparse contents. Its planner groups adjacent
all-zero clusters into holes and other clusters into data segments; a final
partial cluster is classified by its supplied bytes. It allocates only data
segments, constructs VCN-contiguous runs, and writes sparse header/file flags.
**DOCUMENTED / IMPLEMENTED:** [sparse.rs](../../../src/sparse.rs),
`write::write_sparse_file_io`; [sparse_write.rs](../../../tests/sparse_write.rs).

An in-place write into a sparse hole is a different operation. `write_at` preflights
the entire target range and refuses holes or unmapped clusters rather than
allocating behind them. The existence of sparse replacement does not imply
arbitrary hole punching or sparse in-place allocation.

<a id="resident-migration"></a>

## Resident-to-nonresident migration

Promotion allocates clusters, copies the existing value, builds a nonresident
attribute, and replaces the resident header/value in the same record. A general
attribute helper supports types/names as well as the unnamed-data convenience
path. It does not generate `$ATTRIBUTE_LIST` continuation records.
**IMPLEMENTED:** `write::promote_resident_data_to_nonresident_io`,
`promote_attribute_to_nonresident_io`; [write_promote.rs](../../../tests/write_promote.rs).

The transaction boundary matters: allocated clusters must exist before a live
mapping points to them. If a later commit fails, explicit rollback attempts free
the allocation. That rollback is ordinary error handling, not a journal able to
recover after process termination or power loss.

<a id="bitmap-overview"></a>

## Three allocation bitmaps

| Storage | Bit index describes | Set bit means |
| --- | --- | --- |
| Record 6 `$Bitmap:$DATA` | Volume LCN | Cluster allocated |
| Record 0 `$MFT:$BITMAP` | MFT record number | Record slot occupied |
| Directory `$BITMAP:$I30` | Index block ordinal | INDX block in use |

Within a byte, bit `k % 8` of byte `k / 8` represents item `k`; least-significant
bit comes first. None of these maps alone establishes the other two kinds of
ownership. **IMPLEMENTED:** [bitmap.rs](../../../src/bitmap.rs),
[mft_bitmap.rs](../../../src/mft_bitmap.rs), [idx_block.rs](../../../src/idx_block.rs).

<a id="bitmap-update-rules"></a>

### Volume cluster allocator

The allocator finds contiguous free runs with a first-fit scan and a hint, rather
than a general best-fit or locality policy. A request may fail for lack of one
contiguous run despite enough total free clusters. Bitmap reads/writes map through
its own run list. `free_io` refuses protected metadata ranges rather than freeing
whatever LCN a malformed file claims to own.

Protected ranges include boot storage, MFT and mirror extents, log, attribute
catalog, bitmap, security catalog, and upcase storage as discovered by the source.
Fragmented system streams are represented as ranges; the recovery fallback is
bounded rather than declaring the entire volume permanently protected.
**IMPLEMENTED:** `BitmapLocation::covers_the_volumes_own`,
`read::other_protected_metafile_ranges_io`;
[allocation_bounds.rs](../../../tests/allocation_bounds.rs).

On shrink or last unlink, stop the live record referencing discarded storage
*before* clearing its allocation bits. A failure then leaks reserved space, which
is preferable to a live file pointing at space handed to another file. This is
this driver's write-order invariant, not a claim of crash-atomic operation.

### Record allocator and MFT growth

A bitmap can describe more records than `$MFT:$DATA` currently stores. Free-slot
search is bounded by both the bitmap and the initialized record stream:

```
record_limit = min(bitmap_covered_bits, initialized_mft_bytes / record_size)
```

The resident bitmap is reread from record 0 so rollback sees earlier writes.
When no usable slot remains, the code grows the MFT in batches based on 64 records,
allocates a new extent, initializes its storage, adjusts bitmap coverage as needed,
updates record 0's mapping and lengths, and synchronizes its mirror. Insufficient
base-record or bitmap capacity can still refuse growth. **IMPLEMENTED:**
`MftBitmap::record_limit`, `find_or_grow_free_record_io`, `grow_io`.

<a id="bad-clusters"></a>

## Bad-cluster reservation

Record 8 is `$BadClus`; its named `$DATA:$Bad` represents bad-cluster reservations.
The formatter emits a sparse logical stream spanning the cluster address space
without allocating normal content for every VCN. Holes are not bad clusters; an
allocated range is the part requiring reservation. **IMPLEMENTED:** [mkfs.rs](../../../src/mkfs.rs).

Automatic media-error detection, relocating damaged file clusters, and updating
`$Bad` are not implemented. A disk I/O error is not enough information to choose a
Windows-compatible replacement policy or to salvage partially readable content.

<a id="bitmap-reconciliation"></a>

## Reconciliation and unresolved ownership

A complete ownership checker would compare all live stream mappings with `$Bitmap`,
exclude sparse holes, account for continuation records and metadata, and detect
cross-linked clusters. The current `fsck` is not such a scan. Missing mappings,
duplicate owners, and extension traversal prevent an implementation from safely
clearing bits merely because a limited reader did not find an owner.
**OPEN:** Q08. No TRIM/discard or whole-volume bitmap rebuild is implied by the
current allocator or fsck API.

[Previous: MFT records](02-mft-records.md) | [TOC](../ntfs-specification.md) | [Next: Indexes](04-indexes-directories.md)
