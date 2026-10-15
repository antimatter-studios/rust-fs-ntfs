# Features

What this driver does today, what it refuses, and what is coming. **Every
pull request that adds, fixes, refuses or removes behaviour updates its row
here, in the same pull request** (AGENTS.md). The reasoning behind each change
is in [CHANGELOG.md](../CHANGELOG.md); the write plan's item numbers (W2.6 and
so on) are in [future-features.md](future-features.md).

**Since** is the release a row's current state shipped in, with the issue or
pull request the changelog cites for it. Work merged after the last release
is **Unreleased (#N)** until the next one. **Tracking** names the open issue,
or the write-plan item, for anything not finished.

States:

- **Supported**: works, and is checked against Windows (`chkdsk`, `ntfs.sys`
  through the test matrix), ntfs-3g, Linux's ntfs3, or images Windows wrote.
- **Experimental**: works in every test, but is new.
- **Partial**: works for part of the case, and the row says which part.
- **Refused**: recognised and refused by name, rather than misread.
- **Not supported**: neither read nor refused by name.
- **Upcoming**: an open issue with a plan.

## Reading

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| Boot sector, `$MFT`, `$Volume`; both `sectors_per_cluster` encodings | Supported | 0.1.0; encodings 0.5.0 | | `integration.rs`, `boot_sector_geometry.rs` |
| `stat`, resident and non-resident attributes | Supported | 0.1.0 | | `integration.rs`, `native_read_fixtures.rs` |
| Directories: `$INDEX_ROOT` and `$INDEX_ALLOCATION`, looked up by B+tree descent | Supported | 0.1.0; descent 0.5.0 | | `manyfiles.rs`, `deep.rs`, `index_lookup_descent.rs`, `fragmented_indx_fixture.rs` |
| A stale index entry whose record now holds another file | Refused at lookup | 0.5.0 | | `stale_index_entry.rs` |
| File content: resident, non-resident, fragmented | Supported | 0.1.0 | | `large_file.rs`, `native_read_fixtures.rs` |
| Sparse files: holes read as zeros | Supported | 0.1.0 | | `sparse.rs` |
| Bytes past `initialized_size` read as zeros | Supported | 0.5.0 | | `initialized_length.rs` |
| LZNT1-compressed `$DATA` | Supported | 0.3.0 | | `native_read_fixtures.rs` |
| WOF-compressed files (`compact /exe`, Compact OS) | Refused | 0.5.0 | | `wof_read.rs` |
| Encrypted `$DATA` (EFS) | Refused | 0.3.0 | | `write_transform_guards.rs` (writes) |
| `$ATTRIBUTE_LIST`: attributes spread across several MFT records | Supported | 0.3.0; split run lists 0.5.0 | | `native_read_fixtures.rs`, `attribute_list_split.rs` |
| Alternate data streams | Supported | 0.1.0 | | `ads.rs`, `ads_comprehensive.rs`, `list_named_streams.rs` |
| Reparse points: symlinks, junctions, generic | Supported | 0.1.0; readlink contract 0.5.0 | | `readlink.rs`, `symlink_variants.rs`, `readlink_windows_oracle.rs` |
| Extended attributes (`$EA`, `$EA_INFORMATION`) | Supported | 0.1.0 | | `ea_combinatorics.rs`, `list_ea_keys.rs` |
| `$OBJECT_ID`, security ids, every `$STANDARD_INFORMATION` field | Supported | 0.1.0 | | `object_id.rs`, `security_id.rs`, `read_si_full.rs` |
| Bounded self-relative descriptor header and optional owner/group SIDs, raw control and 48-bit authority; ACL/ACE framing validated with raw payloads retained, without policy evaluation | Supported | Unreleased (#615) | #606 | `security_descriptor.rs`, `security_descriptor_windows_oracle.rs` |
| Unicode names, `$UpCase` collation | Supported | 0.1.0 | | `unicode.rs`, `upcase.rs`, `index_name_collation.rs`, `long_names.rs` |
| Volume statistics, label, version | Supported | 0.1.0 | | `volume_stats.rs`, `volume_info_v2.rs`, `volume_label.rs` |
| Volumes Windows was writing to when captured | Supported | 0.6.0 | | `windows_interrupted_fixture.rs` |
| Interrupted-write fixture bootstrap removal | Partial: all 26 Windows interrupted partitions and their capture builder are scrubbed; other fixture families remain | Unreleased (#619) | #487 | `check-interrupted-bootstrap.py`, `interrupted_bootstrap_guard.py`, `scrub-bootstrap.ps1`; existing Windows replay and manifest assertions |
| Corrupt or hostile structures | Refused, without panicking | 0.1.0 | | `corruption_fuzz.rs`, `corruption_resistance.rs`, `read_run_bounds.rs`, `index_root_bounds.rs`, `fuzz_decoders.rs` |

## Checking and recovery

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| `fsck.ntfs` / `fsck::check_io`: dirty flag, `$LogFile`, `$MFTMirr`, the header of every in-use MFT record | Supported | 0.6.0 | | `cli_fsck.rs`, `tests/cli/test-fsck.sh` |
| Seeded operation sequences against an independent byte model and reader | Partial: create, offset write, truncate, duplicate-create refusal; broader operations and Windows grading remain | Unreleased (#578) | #484 | `op_sequence_readback.rs`, `chore test:op-sequence-oracle` |
| `fsck.ntfs` / `fsck::check_io`: each in-use record's attribute chain: lengths, types and their order, names, ids, resident values, run lists against their VCNs and allocated size, the end marker | Supported | 0.10.0 (#515) | | `check_io_reads_attribute_structure.rs` |
| `fsck.ntfs` / `fsck::check_io`: `$Bitmap` against every run list: leaked, cross-linked and in-use-but-free clusters, runs past the volume | Supported | 0.10.0 (#528) | | `check_io_cluster_accounting.rs`, `tests/cli/test-fsck.sh` |
| `fsck.ntfs` / `fsck::check_io`: indexes | Not supported | | | |
| `$LogFile` state, read from every record page | Supported | 0.6.0 | | `logfile_state.rs` |
| `$LogFile` replay on `fsck` and on a read-write mount that asks for it (`mount_rw_replaying_log`, the `_replaying_log` C ABI mounts), as Windows' restart does, for every operation captured Windows logs hold; open transactions rolled back; wrapped logs; small clusters | Supported | 0.8.0 (#137); only when asked 0.10.0 (#513) | | `windows_interrupted_fixture.rs` |
| A read-write mount, a path-based write or `fs.ntfs` over a log holding work, unasked: Windows leaves one after a Fast Startup shutdown or a hibernation too, which a replay corrupts | Refused, with nothing written, naming `fsck.ntfs -y` | 0.10.0 (#513) | #137 | `windows_interrupted_fixture.rs`, `tests/cli/test-write.sh` |
| `UpdateRecordDataRoot` redo; a checkpoint that names no start | Supported | 0.10.0 (#456, #457) | | `windows_interrupted_fixture.rs` |
| `UpdateRecordDataAllocation` redo: a `$Quota` entry charged in an index block, once owners outnumber what the index root holds | Supported | Unreleased (#576) | | `windows_interrupted_fixture.rs` |
| A log holding an operation no capture has held, a prepared or committed transaction not forgotten, or an LFS version other than 2.x | Refused for writing, with nothing written | 0.8.0 (#137) | #137 | `windows_interrupted_fixture.rs` |
| A replay stopped after any write (a power cut), run again | Supported: it reaches the volume an uninterrupted replay does, and the log reads as clean from the moment it starts being emptied | 0.10.0 (#514) | #137 | `windows_interrupted_fixture.rs` |
| A read-only mount of a volume whose log holds work | Partial: read as it is on disk, without replaying | 0.8.0 | #137 | |
| A replay, or a dirty-flag repair, through the read/write callback pair (`FsNtfsBlockdevCfg`), which has no flush callback to order its writes | Refused, with nothing written; the path backends and an fs-core device (`fs_core_device_from_callbacks` with a flush callback) replay, flushing where `fsync` would | 0.10.0 (#518) | #137 | `windows_interrupted_fixture.rs` |
| Clear the dirty flag; reset the log | Supported, for volumes known consistent: fsck clears the flag only when asked (`--clear-dirty`, `FsckOptions::clear_dirty`, `FS_NTFS_FSCK_CLEAR_DIRTY`) and the log holds nothing to redo or undo, and otherwise leaves it for Windows' chkdsk | 0.1.0; left set by default 0.10.0 (#519) | | `fsck.rs`, `capi_fsck.rs`, `capi_fsck_callbacks.rs`, `tests/cli/test-fsck.sh` |
| Repair beyond log replay and the dirty flag | Not supported | | | |

## Writing

Every write test checks its result by remounting and reading back; the
multi-VM test matrix (`test-matrix.json`) has Windows mount, write and
`chkdsk` the volumes.

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| In-place write to existing non-resident `$DATA` | Supported | 0.1.0 | | `write_content.rs` |
| Replace contents: resident, promoted to non-resident when it outgrows the record | Supported | 0.1.0 | | `write_resident_contents.rs`, `write_promote.rs`, `replace_contents.rs` |
| Grow and truncate non-resident `$DATA` | Supported | 0.1.0 | | `write_grow.rs`, `write_truncate.rs` |
| Truncate resident `$DATA` | Not supported | | | |
| Sparse writes (`write_sparse_file`) | Supported | 0.3.0 | | `sparse_write.rs` |
| `write_at`, `truncate`, `grow` on compressed, sparse or encrypted `$DATA` | Refused | 0.5.0 | | `write_transform_guards.rs` |
| `create_file`, `unlink`, `mkdir`, `rmdir` | Supported | 0.1.0 | | `write_create.rs`, `write_unlink.rs`, `write_mkdir.rs`, `write_rmdir.rs` |
| `unlink` of a file's last name when ntfs-3g or Windows spread its attributes over extension records: the extension records and every cluster they map are freed with the base record | Supported | 0.10.1 (#565) | | `ntfs3g_populated.rs`; Windows matrix `win-format-win-write-streams-mac-delete-win-chkdsk` |
| Directories past `$INDEX_ROOT`: entries added and removed, index blocks split, the index gaining levels | Supported, up to 64 index blocks per directory | 0.8.0 (#432) | | `large_directory.rs`, `indx_insert_bounds.rs`, `indx_insert_interior.rs`; Windows matrix `win-format-win-write-many-mac-insert-index-allocation-win-chkdsk` |
| A directory needing more than 64 index blocks (a non-resident `$Bitmap:$I30`) | Refused | 0.8.0 (#432) | | `large_directory.rs` |
| `rename`: same and different length; atomic replace | Supported | 0.1.0; replace 0.3.0 | | `write_rename.rs`, `write_rename_varlen.rs`, `capi_rename_overwrite.rs` |
| Hard links | Supported | 0.1.0 | | `write_link.rs`, `hardlink_scenarios.rs` |
| Alternate data streams: write, delete, promote | Supported | 0.1.0 | | `write_ads.rs`, `write_ads_promote.rs`, `ads_combinatorics.rs` |
| A named stream the file's base record cannot hold: the file is given an `$ATTRIBUTE_LIST` and extension records, as Windows does; the named streams of a file that has a list, whichever implementation wrote it, are written, rewritten and deleted through it, and an extension record left empty is freed | Experimental: a regular file's named streams | Unreleased (#480) | #480 | `attribute_list_creation.rs`, `attribute_list_records_refuse_changes.rs`; Windows matrix `mac-format-mac-write-streams-win-chkdsk`, `mac-format-mac-write-big-streams-win-chkdsk`, `win-format-win-write-streams-mac-write-streams-win-chkdsk` |
| Any other attribute that would outgrow a full base record or be added to or removed from a file with an `$ATTRIBUTE_LIST` (an EA, a hard link's name, a reparse point, an object id, the unnamed `$DATA`); a stream a directory's record cannot hold | Refused, with nothing written | 0.10.0 (#468); streams Unreleased (#480) | #480 | `attribute_list_records_refuse_changes.rs`, `refused_stream_rewrite_keeps_stream.rs`, `attribute_list_creation.rs` |
| Reparse points and symlinks: write and remove | Supported | 0.1.0 | | `write_reparse.rs` |
| Reparse points and object ids entered in `$Extend\$Reparse:$R` and `$Extend\$ObjId:$O`, and taken out with the attribute or the file; a second file refused an object id another has | Supported for removal; insertion into spilled indexes, root promotion and leaf splitting await CI and Windows oracle validation | 0.10.2 (#478, #571); insertion Unreleased (#577) | #571 | `extend_view_indexes.rs`, `view_index_spilled.rs`; Windows matrix `mac-format-mac-symlink-win-chkdsk`, `mac-format-mac-objid-win-chkdsk`, `win-format-win-write-objids-mac-delete-win-chkdsk`, `win-format-win-write-views-mac-write-views-win-chkdsk` |
| Extended attributes: write and remove | Supported | 0.1.0 | | `write_ea.rs`, `field_exhaustion_ea.rs` |
| Timestamps, file-attribute flags, `$OBJECT_ID`, security id | Supported | 0.1.0 | | `write_times.rs`, `write_attrs.rs`, `set_object_id_extended_h.rs`, `security_id.rs` |
| Volume label | Supported | 0.1.3 | | `volume_label.rs` |
| A recycled MFT record keeps counting its sequence number | Supported | 0.5.0 | | `record_sequence_reuse.rs` |
| An allocation the volume cannot honour | Refused before anything is written | 0.5.0 | | `allocation_bounds.rs`, `write_run_bounds.rs` |
| A read-write mount of, or a path-based write to, a dirty volume, or one whose log cannot be replayed in full | Refused | 0.7.0; path-based writes 0.10.0 (#465) | #137 | `windows_interrupted_fixture.rs` |
| A read-write mount of, or `fsck` on, a volume Windows is hibernated on: `hiberfil.sys` begins `hibr` or `HIBR`, or is too short to hold its header | Refused, with nothing written | 0.10.0 (#496) | #137 | `hibernated_volume.rs` |
| `$MFT` self-growth once a resident `$MFT:$Bitmap` is full | Not supported | | W2.6 | |
| Compressed `$DATA` writes | Refused | 0.5.0 | | `write_transform_guards.rs` |
| A write to a volume with an active change journal (`$Extend\$UsnJrnl`) | Partial: no record is appended to `$UsnJrnl:$J`; before the first write the journal is retired (`$Volume` flag `0x0008`), Windows gives it a new id on its next mount, and its readers scan the volume again rather than miss the write. A journal that cannot be retired refuses the write | Unreleased (#574) | #479 | `usn_journal_windows_oracle.rs`; Windows matrix `win-format-win-usn-journal-mac-write-win-usn-retired-chkdsk` |
| Transactional NTFS (TxF) | Not supported: deprecated by Microsoft | | | |
| Resize | Not supported (`not implemented`, exit 3) | 0.6.0 | | `tests/cli/test-fs.sh` |

## Making a filesystem

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| `mkfs.ntfs` / `format_filesystem`: volumes Windows mounts and writes, and `chkdsk /scan` accepts | Supported | 0.1.3 | | `mkfs_roundtrip.rs`, `format_populate_remount.rs`, `cluster_size_matrix.rs`, `test-matrix.json` |
| Volumes ntfs-3g mounts read-write | Supported | 0.6.0 | | |
| What this crate writes, read back identically by Linux's ntfs3: nested and INDX-sized directories, resident, non-resident and sparse files, renames, hard links, unlinks, relative symlinks, at 512-byte and 4 KiB clusters | Supported | 0.10.0 (#542) | | `kernel_ntfs3.rs` |
| Volumes Linux's ntfs3 wrote, read back byte for byte | Supported | 0.10.0 (#542) | | `kernel_ntfs3.rs` |
| 1024-byte MFT records | Supported | 0.8.0 | | `large_directory.rs` |

## Interfaces

| Feature | State | Since | Tracking | Checked by |
|---|---|---|---|---|
| C ABI: path and handle entry points, path and callback transports, fs_core devices | Supported | 0.1.0; handles and fs_core 0.1.3 | | `capi_create.rs`, `capi_write_content.rs`, `capi_handle_rw.rs`, `capi_fs_core_rw.rs`, `capi_fsck_callbacks.rs` |
| C ABI errno: 0 after success, decided where the error is raised | Supported | 0.7.0 | | `errno_companion.rs`, `errno_as_data.rs`, `errno_kinds.rs` |
| Rust facade (`facade::Filesystem`), typed errors | Supported | 0.1.0; typed errors 0.7.0 | | `facade.rs`, `error_paths.rs` |
| `fs.ntfs` `ls`, `read`, `get`/`info`, `write`, `mkdir`, `set label` (`--features cli`) | Supported | 0.6.0 | | `cli_verbs.rs`, `tests/cli/test-fs.sh`, `tests/cli/test-write.sh` |
| `fs.ntfs` `rm`, `rmdir` (`--features cli`) | Supported | 0.10.0 | #550 | `tests/cli/test-rm.sh`, matrix `cli-empty-directory-win-verify-chkdsk` |
| `fs.ntfs` writing verbs on a volume whose log holds work | Refused | 0.8.0 | | `tests/cli/test-write.sh` |
| `rust-fs-ntfs doctor`, man pages, shell completions, one program on `PATH` | Supported | 0.6.0; one program 0.10.0 (#454) | | `cli_verbs.rs`, `one_binary_on_path.rs` |
