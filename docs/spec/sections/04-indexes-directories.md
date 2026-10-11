[Previous: Allocation](03-data-runs-bitmap.md) | [TOC](../ntfs-specification.md) | [Next: Recovery](05-logfile-journal.md)

# 4. Names, indexes, and directories

<a id="overview"></a>

## Overview

A directory index maps a filename key to a file reference. The child record also
stores its filename and parent reference in `$FILE_NAME`. These are separate
representations of a relationship. Updating only one side can leave a name that
cannot be followed or a child with no reachable parent entry.
**IMPLEMENTED:** [index_io.rs](../../../src/index_io.rs),
[write.rs](../../../src/write.rs); Microsoft's
[stream types](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams)
identify `$FILE_NAME`, `$INDEX_ROOT`, and `$INDEX_ALLOCATION`.

<a id="file-name-attr"></a>

## Filename value

The same value layout forms the filename key in `$I30` entries.
**IMPLEMENTED:** `record_build` filename builders, `index_io::build_file_name_index_entry`.

| Value offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | Parent file reference |
| `08` | 8 | Creation time copy |
| `10` | 8 | Content modification time copy |
| `18` | 8 | MFT change time copy |
| `20` | 8 | Access time copy |
| `28` | 8 | Allocated size copy |
| `30` | 8 | Logical data size copy |
| `38` | 4 | File flags |
| `3C` | 4 | EA/reparse-related field, not generally interpreted by our reader |
| `40` | 1 | Name length in UTF-16 units |
| `41` | 1 | Namespace |
| `42` | `2 * length` | UTF-16LE filename |

Filename timestamps and sizes are duplicated metadata, not the stream's allocation
map or EOF authority. `read_stat` gets primary size from `$DATA` and timestamps
from standard information. A mutation can leave filename copies stale; the exact
Windows refresh rules remain an explicit question. **IMPLEMENTED / OPEN:**
[read.rs](../../../src/read.rs) `read_stat`, Q15.

<a id="fn-directory-bit"></a>

### Directory and view-index bits

The source uses `10000000` to identify directories in filename keys, and
`20000000` for NTFS view-index files. These are not interchangeable with the
ordinary Win32 directory bit `00000010` or the FILE header's directory flag
`0002`. Listing uses the filename-key directory bit without reading the child.
**IMPLEMENTED:** `read::read_dir_entries`, `record_build` flag constants.

<a id="namespaces"></a>

## Namespaces and aliases

| Value | Interpretation in source |
| --- | --- |
| 0 | POSIX namespace |
| 1 | Win32 namespace |
| 2 | DOS short-name alias |
| 3 | Combined Win32/DOS name |

The builder uses namespace 3 for names passing its DOS-compatible length/character
heuristic, otherwise 0. Leading/trailing dots and multiple-dot extensions fail
that heuristic. Namespace 0 does not make the current path resolver case-sensitive.
It is an on-disk name classification, not a request to change collation.
**IMPLEMENTED:** `record_build::fn_namespace_for`, [read.rs](../../../src/read.rs).

<a id="dos83-generation"></a>

### Short-name generation

There is no generated `LONGFI~1.TXT` companion algorithm in the runtime writer.
The reader suppresses namespace-2 shadow entries from public enumeration. Existing
short aliases and namespace combinations still require careful link accounting:
the count of filename attributes is not automatically the count of independent
hard links. No speculative three-tier Windows 8.3 collision algorithm is prescribed
here. **IMPLEMENTED / OPEN:** Q15.

<a id="index-root"></a>

## Resident index root

A directory's `$INDEX_ROOT:$I30` contains the root node. Its value begins with a
16-byte root prefix followed by a 16-byte node header. A small directory can keep
its keys entirely inside its FILE record. **IMPLEMENTED:** [index_io.rs](../../../src/index_io.rs),
[record_build.rs](../../../src/record_build.rs).

<a id="index-root-header"></a>

### Root prefix

| Value offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | Indexed attribute type; `$FILE_NAME` = `30` |
| `04` | 4 | Collation rule; filename = 1 |
| `08` | 4 | Index block size in bytes |
| `0C` | 1 | Cluster-related block geometry field emitted by builder |
| `0D` | 3 | Reserved |

The writer derives block geometry from the volume. Hardcoding 4096 in a newly
created directory when the volume declares another size risks a chkdsk mismatch.
The signed/small-block encoding and its VCN interpretation need special attention
when clusters are larger than index blocks; see Q09.

<a id="index-header"></a>

### Node header

Offsets here are relative to the node header, not to the attribute or FILE record.

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | First entry offset |
| `04` | 4 | Used node bytes, including header-relative prefix and sentinel |
| `08` | 4 | Allocated node bytes |
| `0C` | 1 | Flags; bit 1 value (`01`) indicates child nodes |
| `0D` | 3 | Reserved |

The node header is at root-value offset `10` or INDX-block offset `18`.
Entry walking must stay inside the value/node bounds, not merely the entire record
buffer. An unreadable root is an error, not an empty directory.
**IMPLEMENTED:** `index_root_value_end`, `lookup_node`, `collect_entries`.

<a id="index-allocation"></a>

## External index blocks

When keys no longer fit the root, `$INDEX_ALLOCATION:$I30` stores `INDX` blocks.
`$BITMAP:$I30` identifies occupied block ordinals. Its resident or nonresident
form can be read; nonresident bitmap reading is not the same as growing its storage.
**IMPLEMENTED:** [idx_block.rs](../../../src/idx_block.rs) `load_for_directory_io`.

<a id="indx-record"></a>

### INDX envelope

| Block offset | Bytes | Field |
| --- | --- | --- |
| `00` | 4 | `INDX` signature |
| `04` | 2 | USA offset |
| `06` | 2 | USA word count |
| `08` | 8 | LSN |
| `10` | 8 | Block VCN |
| `18` | 16 | Node header |
| header-relative | variable | Entries, ending in sentinel |

INDX protection uses the same restored-buffer discipline as FILE records.
A block may span several clusters and several physical runs. The actual read/write
path maps it piecewise through every contributing run, applies USA to the assembled
buffer, and scatters the encoded result. A helper that promises a *single* physical
offset refuses a run-straddling block. Do not mistake that helper restriction for
a restriction of piecewise block I/O.
**IMPLEMENTED:** `map_indx_block`, `read_raw_indx_block_io`, `update_indx_block_io`;
[foreign fragment evidence](../../testing/windows-full-matrix-2026-10-10/README.md).

<a id="index-bitmap"></a>

### Bitmap ordinals and VCNs

A bitmap bit describes a block ordinal. For geometries where blocks cover an
integral number of clusters, `block_vcn = ordinal * block_size / cluster_size`.
The source enumerator implements this arithmetic. On a geometry with a cluster
larger than the block, it can collapse different ordinals to the same cluster VCN.
Alternative subcluster/sector-unit interpretation is a compatibility gap requiring
Windows evidence, not something to infer from this formula. **IMPLEMENTED / OPEN:** Q09.

<a id="index-entry"></a>

## Index entry

Filename index entry offsets are relative to each entry:

| Offset | Bytes | Field |
| --- | --- | --- |
| `00` | 8 | Target file reference |
| `08` | 2 | Total entry length |
| `0A` | 2 | Key length |
| `0C` | 2 | Flags: `0001` child VCN, `0002` last entry |
| `0E` | 2 | Reserved |
| `10` | variable | Filename key |
| final 8 bytes | 8 if child flag | Child VCN |

The last entry is a routing sentinel, not a filename. It can contain the rightmost
child VCN. The child pointer is at the end of the entry, not invariably immediately
after a fixed-size key. View-index entries reuse the envelope with data offset and
length fields in the first bytes rather than a file reference.
**IMPLEMENTED:** `lookup_node`, `parse_node`, `build_file_name_index_entry`;
[mkfs.rs](../../../src/mkfs.rs) view-index builders.

<a id="btree-node"></a>

## Tree traversal and enumeration

The lookup code compares sorted keys and follows the child for the relevant interval,
including the rightmost sentinel child. An interior node can contain actual keyed
entries as well as child pointers; assuming all user keys live only in leaves loses
entries. Each descended VCN must be allocated and not already visited.
**IMPLEMENTED:** `read::lookup_in_directory`, `index_io::lookup_node`.

Enumeration currently collects root keys and keys from all bitmap-allocated INDX
blocks, then filters DOS aliases. This differs from lookup's routed tree walk:
a bitmap-allocated but unreachable block can affect listing. No globally sorted
output, stale-reference validation for every entry, or orphan reconstruction is
promised by this enumeration algorithm.
**IMPLEMENTED:** `read_dir_entries`; [stale_index_entry.rs](../../../tests/stale_index_entry.rs).

<a id="collation-rules"></a>

## Collation

Default filename comparison maps each UTF-16 code unit through the volume's
`$UpCase` table, compares mapped units lexicographically, then compares length.
This is not host Unicode case folding, Unicode normalization, or a mapping that
expands one code point into multiple characters. Path resolution requires a usable
on-volume table. The public create/rename paths load it for duplicate checks and ordering. An
internal insertion helper reloads it with `.ok()` and has an ordinal fallback;
low-level helper behavior is therefore not an unconditional guarantee that all
writes fail closed on an unreadable table.
**IMPLEMENTED:** `index_io::compare_names`, `upcase::UpcaseTable`, `write` insertion paths.

An ordinal comparator exists, but is not wired through all directory paths to
provide Windows/WSL case-sensitive-directory behavior. The source comments' proposed
flag location has not been established; Q10 asks for the real on-disk metadata and
Windows control-query evidence.

### Other collation constants

The formatter emits filename `01`, unsigned-long `10` for `$SII`/`$Q`, SID `11`
for quota-owner `$O`, security-hash `12` for `$SDH`, and unsigned-longs `13`
for object/reparse indexes. These constants and emitted index shapes are
**IMPLEMENTED** in `mkfs.rs`; complete comparison algorithms for all those key
types are not implemented by the filename comparator.

<a id="i30-directory"></a>

## Directory growth and mutation

The current writer promotes a full resident root to external storage and plans
splits along a routed path. Full leaves and interior nodes can split; promoted keys
propagate upward, and a full root can add a tree level. Plans preflight record space
and allocations before publishing a new file's name. Reusable mapped blocks are
considered before allocation growth. **IMPLEMENTED:** `write::plan_index_split_io`,
`commit_index_split_io`, `promote_parent_index_io`.

Growth remains bounded by existing resident bitmap capacity and mapping/header room.
The block allocator refuses growing a nonresident `$BITMAP:$I30`; it also refuses
when resident bitmap enlargement would be required. There is no general tree
merge/rebalance deletion implementation. Removal handles supported resident/leaf
cases and refuses interior-node deletion cases requiring restructuring.
**IMPLEMENTED:** `IndexBlockAllocator::new_block`, `remove_parent_index_entry_io`.

A retained Windows test inserted a host-created file into a Windows directory with
256 original files; Windows enumerated all 257 required names and both chkdsk modes
scanned cleanly. This proves that specific insertion case, not unrestricted tree
mutation. **OBSERVED:** [targeted result](../../testing/windows-index-insertion-2026-10-10/README.md).

<a id="i30-system-skeleton"></a>

## System filename keys

The formatter's root system-file keys and `$Extend` child keys use a skeleton
filename value: parent reference, namespace, and name are filled while numeric
metadata copies are zero. The in-record filename on the system file is populated
separately. This intentional difference is implemented by
`mkfs::build_skeleton_fn_stream`; its source comment identifies the Windows-format
byte comparisons behind the profile. Do not “repair” these zeros merely because
an ordinary user filename has cached times/sizes in the same fields.
**IMPLEMENTED / historical observation:** [mkfs.rs](../../../src/mkfs.rs).

<a id="i30-dotdot"></a>

## Root and parent navigation

The root is record 5. The formatter includes its system-file keys and the root's
own `.` representation. `resolve_path` treats empty components and `.` as staying
put, follows a filename parent reference for `..`, and keeps root as its own parent.
API dot/dotdot listing behavior belongs to the facade; it is not evidence that every
subdirectory contains literal `.` and `..` keys. **IMPLEMENTED:** [read.rs](../../../src/read.rs),
[facade.rs](../../../src/facade.rs), [readdir_dots.rs](../../../tests/readdir_dots.rs).

A directory may also contain named `$DATA`; the directory index and an ADS are
separate attributes. Adding an ADS does not make it an indexed child.

<a id="orphan-recovery"></a>

## Orphans and unresolved relationships

An allocated record with a filename but no reachable parent key, and an allocated
INDX block not linked by tree routing, are different inconsistencies. Recovering
both requires a complete graph/ownership scan. Current lookup, listing, and fsck
are not an orphan recovery engine. **OPEN:** Q08, Q11, Q15.

[Previous: Allocation](03-data-runs-bitmap.md) | [TOC](../ntfs-specification.md) | [Next: Recovery](05-logfile-journal.md)
