[Previous: Payloads](06-special-streams.md) | [TOC](../ntfs-specification.md)

# 7. File lifecycle and behavioral contracts

This chapter follows changes across structures, so the field tables can be read
as a coherent filesystem. Behavior is **IMPLEMENTED** at the audited revision
unless an independent observation is explicitly linked. The choices below describe
this driver; they do not prescribe all policies of `ntfs.sys`.

<a id="format-profile"></a>

## Initial formatter profile

The current formatter accepts power-of-two clusters from 512 to 65536 bytes and
power-of-two FILE records from 512 to 16384 bytes, with 512-byte logical sectors.
It requires at least 1024 clusters, sufficient device bytes, and nonoverlapping
metadata reservations. This is narrower than the boot reader's large-cluster
acceptance. It rounds requested capacity down to complete clusters and uses that
formatted extent for sector count and backup placement.
**Source:** [mkfs.rs](../../../src/mkfs.rs) `format_filesystem`.

Let `C` be cluster size and `R` record size. Its layout planner computes:

| Region | Placement / reserved clusters |
| --- | --- |
| Boot | Start 0; `ceil(8192 / C)` clusters |
| MFT | LCN `max(4, ceil(8192 / C))`; `ceil(128 * R / C)` clusters |
| Log | Immediately after MFT; `ceil(3B0000 / C)` clusters |
| Volume bitmap | After log; `ceil(ceil(cluster_count / 8) / C)` clusters |
| Upcase | After bitmap; `ceil(131072 / C)` clusters |
| Attribute definitions | After upcase; space reserved for 2560 bytes |
| MFT bitmap | Next cluster; nonresident initial storage |
| SDS | Next two clusters, primary then mirror |
| Root INDX | Next mapped reservation if root keys do not fit resident storage |
| MFT mirror | LCN `floor(cluster_count / 2)`; enough clusters for exactly four records |
| Backup boot | Final 512-byte sector of formatted extent; final cluster reserved |

These positions are implementation choices, not mandatory NTFS LCNs. Reserved
metadata is reflected in the volume bitmap, and bits beyond the final valid
cluster in the last bitmap byte are set. The MFT bitmap marks system slots and
`$Extend` children as occupied; clear bits outside initialized MFT storage do not
make records allocatable. The writer initializes critical regions without wiping
the whole device. It supplies the clean log profile, embedded upcase table, sparse
SDS mirror mapping, and root/Extend directory relationships from the earlier
chapters, then synchronizes writes.

For a cluster-aligned 256 MiB extent, 4096-byte clusters and 1024-byte FILE
records, the source arithmetic gives the following worked layout:

```
cluster_count = 65536
MFT           = LCN 4, 32 clusters (128 initial records)
LogFile       = LCN 36, 944 clusters
Bitmap        = LCN 980, 2 clusters
UpCase        = LCN 982, 32 clusters
AttrDef       = LCN 1014, 1 cluster
MFT bitmap    = LCN 1015, 1 cluster
SDS copies    = LCNs 1016 and 1017 (logical offsets 0 and 0x40000)
Root INDX     = LCN 1018 if needed
MFT mirror    = LCN 32768, 1 cluster for 4 records
Backup boot   = byte 0x0FFFFE00
```

This is a calculated example from the layout code, not a new raw-disk observation.
Root resident capacity depends on record size; a 1024-byte-record profile can need
an external root node immediately after formatting. Formatter metadata omissions
and corruptions have independent historical chkdsk evidence in the
[bug catalog](../../mkfs-bug-catalog.md).

<a id="path-to-bytes"></a>

## From a pathname to bytes

For `/reports/notes.txt`, the native reader:

1. Parses geometry and loads record 10's upcase table by record number.
2. Starts at root record 5 and looks for the folded key `reports` in `$I30`.
3. Descends allocated INDX child VCNs if needed, checking routing cycles.
4. Reads the referenced record and checks its nonzero reference sequence.
5. Confirms that the parent for the next component is a directory.
6. Finds `notes.txt`, follows its checked reference, and locates the requested
   attribute through an attribute list if present.
7. Copies a resident value or translates stream VCNs to physical LCNs.
8. Returns only initialized content, zero-filling holes and the uninitialized
   logical tail; uses the LZNT1 unit decoder when the stream is compressed.

**Source:** [read.rs](../../../src/read.rs) `resolve_path`, `lookup_in_directory`,
`locate_attribute`, `read_attribute_range`.

Leading `/` is optional; empty components and `.` are ignored; `..` uses a parent
filename reference, with root staying root. This is a library path grammar, not
Windows drive-letter, UNC, extended-path, or NT object-manager parsing.
The reader does not implement automatic symbolic-link traversal at each component.

<a id="create"></a>

## Create a file or directory

Creation checks the basename, resolves the parent, loads the upcase table, and
refuses a collating duplicate. It checks index insertion capacity before writing
a new FILE record. It finds an initialized free MFT slot or grows the MFT, marks
the allocation, builds a record with the slot's new sequence and current parent
reference, applies USA, writes it, then publishes the parent index key.
**Source:** [write.rs](../../../src/write.rs) `create_file_io`, `mkdir_io`,
`preflight_parent_insert_io`; [mft_bitmap.rs](../../../src/mft_bitmap.rs).

A normal file begins with standard information, filename, and empty resident
unnamed data. A directory begins with standard information, filename, and an
empty `$INDEX_ROOT:$I30`, with geometry derived from the volume; it has no default
file-content `$DATA` just because it is a directory. Adding a named stream later
is possible. Creation consumes both record capacity and directory-key capacity.

If a later step fails, rollback distinguishes “bitmap only” from “record written.”
It can return an allocated bit or retire a newly written record, and reports a
rollback failure along with the initiating error. Explicit error rollback has no
opportunity to run after power loss. There is no runtime metadata WAL for creation.

### Name acceptance

The basename validator refuses empty names, `.`/`..`, path separators, colon,
`* ? " < > |`, control characters below `20`, and trailing spaces or periods.
Builders cap filename lengths at 255 UTF-16 code units. These API restrictions
are narrower than every name historically storable on disk and do not implement
all Win32 reserved-device-name rules. Avoid conflating the disk namespace with
Win32 normalization or with our Rust string interface.
**Source:** `validate_basename`, [record_build.rs](../../../src/record_build.rs).

<a id="write-and-vdl"></a>

## Write and initialize data

`write_at` is an in-place operation on ordinary nonresident unnamed data. It
requires the write end to fit current logical EOF, refuses transformed data, and
preflights mapped target clusters. It does not implicitly extend EOF or allocate
sparse holes. A zero-byte write returns zero without changing initialized length.
**Source:** `write_at_by_record_number_io`;
[capi_zero_length_writes.rs](../../../tests/capi_zero_length_writes.rs).

Suppose a stream has EOF 12000 and initialized length 1000. A write at byte 9000
must not expose whatever stale disk bytes happened to occupy 1000–8999. The driver
zeroes allocated parts of that gap, writes the supplied content, synchronizes
storage, then advances initialized length through the new write. Holes remain
logical zeros. Merely increasing initialized length would expose unwritten bytes.
**Source:** `zero_fill_range_io`, `write_at_by_record_number_io`.

A resident whole-value rewrite resizes within its FILE record. When it returns a
specific capacity outcome, `write_file_contents_io` promotes to nonresident data.
An unrelated I/O or format error is not treated as a capacity signal and retried
through destructive fallback. This dispatcher is distinct from an in-place write
against an already nonresident stream.

<a id="resize"></a>

## Resize and replace contents

Growing nonresident data finds/allocates a contiguous extent for required additional
clusters, extends/coalesces mapping pairs as supported, updates allocated and
logical lengths, and leaves newly added logical bytes uninitialized so reads return
zeros. It refuses a mapping list too large for its existing record allocation.
**Source:** `grow_nonresident_by_record_number_io`, `grow_commit_io`.

Shrinking trims or splits runs, commits the shorter mapping and lengths, clamps
initialized length, then frees detached allocated runs. Sparse runs do not become
cluster-free requests. `truncate` dispatches a larger requested size to growth,
and equal size is a no-op. This low-level helper refuses resident data; other
whole-value paths support resident replacement. There is no automatic shrink-to-
resident conversion.
**Source:** `truncate_by_record_number_io`, `split_runs_for_shrink`.

`replace_file_contents_io` dispatches resident replacement separately, otherwise
resizes the existing nonresident stream and writes the new bytes. If growth fails
specifically because another mapping pair will not fit, it can empty the file then
retry growth as one run. If that second allocation fails, the function reports that
the file is now empty. This is a material failure mode: a replacement is not
promised to preserve old contents after every error or be an atomic exchange.
**Source:** `replace_file_contents_io`; [replace_contents.rs](../../../tests/replace_contents.rs).

Sparse whole-file replacement plans zero/data segments and updates sparse state.
That is separate from preserving an existing sparse layout under ordinary writes.

<a id="hard-links"></a>

## Add and remove a hard link

A hard link adds another parent/name relationship to the same FILE record; it does
not copy its unnamed data or ADS. The writer refuses hard links to directories,
checks the new parent's duplicate key, adds a filename attribute, increments link
count, and inserts a parent key using the existing target reference. Available
base-record space limits the number of links independently of free clusters.
**Source:** `link_io`; [write_link.rs](../../../tests/write_link.rs).

Removing one of several links removes the matching parent key and matching filename
attribute and decrements count. Data and the record remain allocated. Only the
last link takes the storage-retirement path. The writer matches parent references
including sequence; a filename under another parent is not interchangeable.
DOS namespace aliases complicate the correspondence between filename attributes
and user-visible links; exhaustive foreign alias behavior remains Q15.

<a id="rename"></a>

## Rename and replace a name

The rename API takes a new basename in the same parent. It does not implement a
cross-directory move. Both the parent key and the child's filename attribute must
change. Duplicate checks use upcase collation, and distinguish separate entries
that happen to reference the same hard-linked record.
**Source:** `rename_same_length_io`, `rename_replace_io`;
[write_rename_varlen.rs](../../../tests/write_rename_varlen.rs).

Equal-*UTF-16-unit* length is the in-place fast path, not equal UTF-8 byte length.
NTFS requires the parent's keys to stay in collation order after a key change, but
the fast path rewrites the name inside the existing entry and does not move it.
A new name that sorts elsewhere leaves the index out of order, and a routed lookup
can no longer find the file: **open defect #466**. Variable-length
rename can resize the child's attribute and replace the resident root entry, but
refuses a parent with `$INDEX_ALLOCATION` overflow (#474). That includes a new
volume's root whenever the formatter had to give it an external index block.
Supported same-length overflow cases do not establish unrestricted rename across
all tree nodes.

Replacement checks source/destination types and refuses a nonempty destination
directory. A destination already referencing the source record is a terminal no-op.
Unsupported parent cases are checked before deleting an existing destination.
The implementation saves/restores supported earlier writes when later steps fail,
but replacement can retire the destination before finishing the source rename.
It is a sequence of durable mutations, not an atomic POSIX rename transaction.

<a id="remove"></a>

## Unlink and remove directories

For the last file link, the writer removes the parent key, retires the FILE record's
in-use state, releases its nonresident storage, then frees the MFT bitmap slot.
Mappings are decoded before the cluster-free loop; protected-metadata checks prevent
freeing system storage attributed to a corrupt file. There is no complete global
cross-link ownership proof before every free.
**Source:** `unlink_io`, `remove_file_record_io`, `free_all_nonresident_runs_io`;
[write_unlink.rs](../../../tests/write_unlink.rs).

A failure after parent removal can leave a unreachable allocated record. A failure
after retirement can leak clusters. The ordering avoids publishing a live record
that still points at clusters already reallocated elsewhere, but no journal makes
all steps atomic. Extension records and all special catalog entries are not fully
reclaimed by a base-record walker.

`rmdir` requires no real resident root entries and refuses an external index even
if its keys have been deleted, since it does not collapse that tree to prove a
supported empty-directory state. Root removal is refused. `remove` dispatches
between file and directory behavior; it is not recursive deletion.
**Source:** `remove_dir_record_io`, `remove_io`;
[write_rmdir.rs](../../../tests/write_rmdir.rs).

<a id="metadata"></a>

## Metadata updates and catalogs

Timestamp setters operate on standard information. Attribute-flag setters alter
its mask and reject overlapping add/remove requests. Neither operation maintains
every cached filename-key copy. Setting a security ID assumes the catalog entry
exists. Object/reparse attribute setters do not maintain their `$Extend` indexes.
EA setters rebuild the local summary and payload together in the record buffer.
Label updates additionally synchronize the mirrored volume record.
**Source:** [write.rs](../../../src/write.rs).

These differences explain why “can emit a structure” and “maintains all related
NTFS invariants” must be separate coverage columns.

<a id="mount-and-recovery"></a>

## Mount and recovery

Read-only mount validates geometry and volume metadata through the native reader.
Writable facade/C-ABI mount prepares the restart state: a dirty flag refuses mount,
a readable supported pending log is replayed, and an unestablished/unsupported log
refuses it. A clear dirty bit never bypasses log inspection.
**Source:** [facade.rs](../../../src/facade.rs) `mount`, `mount_rw`,
[lib.rs](../../../src/lib.rs) `prepare_rw_mount`,
[fsck.rs](../../../src/fsck.rs) `prepare_rw_mount_io`.

Explicit `fsck_io` can replay pending work then clear the dirty flag. Its read-only
check scans bitmap-allocated FILE records for structural readability and compares
the declared mirror records byte-for-byte, in addition to flag/log state. It does not
reconstruct indexes, scan all cluster owners, relocate media defects, or prove that
all user files are intact. Explicit low-level `clear_dirty` and `reset_logfile`
are narrower operations whose caller must establish consistency independently.
Reset discards log contents and does not replay them.

The writable mount also attempts the source's narrow 1.2-to-3.1 volume-version
field upgrade, clearing its upgrade-on-mount flag. Failure is logged as a warning.
Writing version bytes is not evidence that every historical metadata structure has
been converted or that old-volume compatibility is complete. Low-level write
helpers called directly do not all pass through mount preparation.

<a id="durability"></a>

## Durability, exclusivity, and I/O

The general FILE update primitive reads, restores fixups, validates the header and
in-use flag, invokes a mutator, encodes fixups, writes the record, and calls `sync`.
It does not log the change into `$LogFile`. Later-record rollback writes saved clean
bytes with a fresh update sequence rather than pretending no write happened.
**Source:** [mft_io.rs](../../../src/mft_io.rs) `update_mft_record_io_typed`, `restore_mft_record_io`.

The device must have one exclusive writer. The library does not lock the volume
against another process/driver, and a read-modify-write can lose another writer's
change. Callbacks and fs-core transports delegate device ownership and persistence
to the host. The callback transport inherits a no-op `sync`, whereas the fs-core adapter calls
the device flush. Path I/O calls the file sync operation. The host's contract
determines whether callback writes are stable; do not equate all transports'
barriers with file `fsync`.
**Source:** [block_io.rs](../../../src/block_io.rs),
[fs_core_bridge.rs](../../../src/fs_core_bridge.rs), the device contract in `lib.rs`.

No device-independent promise makes an arbitrary two-byte write atomic on all
storage. Nor does each individually synchronized record make a multi-record
operation atomic. The source's error rollback, supported Windows replay, and the
absence of a runtime WAL should be assessed separately.

<a id="errors"></a>

## Error and text boundaries

Malformed storage generally reports I/O errors; invalid API arguments, nonexistent
names, collisions, wrong file/directory type, insufficient capacity, and explicit
refusal use distinct typed errors. The C ABI translates error kinds to errno and
resets the last errno on each entry. Its message can remain from an earlier failure
after a successful call. A directory-iteration end marker is not a stale failure.
These are library contracts rather than bytes stored by NTFS.
**Source:** [error.rs](../../../src/error.rs), [lib.rs](../../../src/lib.rs);
[errno_companion.rs](../../../tests/errno_companion.rs).

NTFS stores UTF-16 code units. The public interface often accepts UTF-8 strings
and some disk-sourced conversions are lossy. Preserve raw bytes/code units for
forensic identity; a displayed replacement character is not necessarily the original
name. The library does not promise lossless round-trip for every malformed UTF-16
name or full Windows case-sensitive-directory behavior.

[Previous: Payloads](06-special-streams.md) | [TOC](../ntfs-specification.md)
