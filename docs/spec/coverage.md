# Source and evidence coverage

Audit: 2026-10-11, source `ee4f530628f507976ddac261906f389823b99c26`.
This map indexes the production modules, their format/behavior coverage, and
regression evidence. A named test is not a claim that it passed in this session.
A formatter, reader, mutator, and independent oracle are separate evidence types.

## Module inventory

| Production source | Specification location | Encoded information / important limitation | Regression or oracle entry point |
| --- | --- | --- | --- |
| [mft_io.rs](../../src/mft_io.rs) | §1, §2, §7 | Geometry, addressing, USA, reuse, mirror sync, RMW durability; single-offset MFT boundary refusal | [mft_io](../../tests/mft_io.rs), [boot geometry](../../tests/boot_sector_geometry.rs), [mirror freshness](../../tests/mftmirr_freshness.rs) |
| [record_build.rs](../../src/record_build.rs) | §2, §4, §6 | FILE, SI, FN, normal/sparse attributes, IDs and reparse builders | Module tests; [resident threshold](../../tests/resident_threshold.rs) |
| [attr_io.rs](../../src/attr_io.rs) | §2, §6 | Attribute offsets/types/flags, iteration/diagnostics; termination does not always distinguish malformed data (Q22) | Module tests; [corruption resistance](../../tests/corruption_resistance.rs) |
| [attr_resize.rs](../../src/attr_resize.rs) | §2, §7 | In-record growth, replacement, sorting, IDs; no extension emission | [attr resize](../../tests/attr_resize.rs) |
| [data_runs.rs](../../src/data_runs.rs) | §3 | Mapping-pair codec, signed deltas, VCN translation, hole checks | [data runs](../../tests/data_runs.rs) |
| [bitmap.rs](../../src/bitmap.rs) | §3 | Cluster bitmap mapping, first-fit allocation/free, protected ranges | [bitmap](../../tests/bitmap.rs), [allocation bounds](../../tests/allocation_bounds.rs) |
| [mft_bitmap.rs](../../src/mft_bitmap.rs) | §2, §3 | Resident/nonresident record bitmap, initialized record limit, MFT growth | Module tests; [record reuse](../../tests/record_sequence_reuse.rs) |
| [index_io.rs](../../src/index_io.rs) | §4 | Root/block nodes, keys, routing, collation, entry edits/splits | [root bounds](../../tests/index_root_bounds.rs), [descent](../../tests/index_lookup_descent.rs) |
| [idx_block.rs](../../src/idx_block.rs) | §4 | Index mapping, bitmap reads, piecewise fragmented block I/O, USA | [fragmented fixture](../../tests/fragmented_indx_fixture.rs) |
| [upcase.rs](../../src/upcase.rs) | §4, §6 | On-volume code-unit collation, captured formatter table | [upcase](../../tests/upcase.rs), [unicode](../../tests/unicode.rs) |
| [read.rs](../../src/read.rs) | §1–§7 | Native resolution, values/ranges/stat/listing, attribute lists, WOF refusal, protection discovery | [native fixtures](../../tests/native_read_fixtures.rs), [split refusal](../../tests/attribute_list_split.rs), [stale entries](../../tests/stale_index_entry.rs) |
| [compression.rs](../../src/compression.rs) | §6 | LZNT1 chunks/tokens/bounds; decode only | Module tests; [native compressed fixture](../../tests/native_read_fixtures.rs) |
| [sparse.rs](../../src/sparse.rs) | §3, §7 | Whole-content zero/data segment planner | [sparse write](../../tests/sparse_write.rs) |
| [ea_io.rs](../../src/ea_io.rs) | §6 | EA packing, compact/query lengths, summary, byte-name matching | [EA combinatorics](../../tests/ea_combinatorics.rs), [EA exhaustion](../../tests/field_exhaustion_ea.rs) |
| [sds.rs](../../src/sds.rs) | §6 | Descriptor hash/header/padding/mirror; small catalog serialization | Module vectors; [mkfs round trip](../../tests/mkfs_roundtrip.rs) |
| [mkfs.rs](../../src/mkfs.rs) | §1–§6 | Initial metadata, system records, boot reservation, clean log, view indexes, quota defaults, AttrDef | [mkfs round trip](../../tests/mkfs_roundtrip.rs); retained Windows matrix |
| [logfile.rs](../../src/logfile.rs) | §5 | Empty/clean/pending state, restart/page/checkpoint inspection | [logfile state](../../tests/logfile_state.rs) |
| [logfile_replay.rs](../../src/logfile_replay.rs) | §5 | Supported LFS 2 analysis, redo, undo, target plans; unknown states/opcodes refused | [Windows interrupted fixtures](../../tests/windows_interrupted_fixture.rs) |
| [fsck.rs](../../src/fsck.rs) | §5, §7 | Allocated-record and mirror check, flags/version helpers, bounded reset, replay application, mount preparation; no full ownership scan | [CLI fsck](../../tests/cli_fsck.rs), Windows interrupted fixtures |
| [write.rs](../../src/write.rs) | §2–§7 | Metadata, content, growth/promotion, index splits, create/link/rename/remove, EAs/IDs/reparse/ADS, rollback | `tests/write_*.rs`, [transform guards](../../tests/write_transform_guards.rs), [replace contents](../../tests/replace_contents.rs) |
| [block_io.rs](../../src/block_io.rs) | §1, §7 | Random access, device size, path/callback durability boundary | [callback RW](../../tests/capi_handle_rw.rs) |
| [fs_core_bridge.rs](../../src/fs_core_bridge.rs) | §7 | Device transport, host ownership/persistence | [fs-core RW](../../tests/capi_fs_core_rw.rs) |
| [facade.rs](../../src/facade.rs) | §7 | Native RO/RW mount and operation dispatch | [facade](../../tests/facade.rs) |
| [lib.rs](../../src/lib.rs) | §6, §7 | C ABI/mount routing, readlink/tag classification, errors and text conversion | `tests/capi_*.rs`, [errno](../../tests/errno_companion.rs), [Windows readlink](../../tests/readlink_windows_oracle.rs) |
| [error.rs](../../src/error.rs) | §7 | Typed failure kinds/errno; API contract rather than disk structure | [errno kinds](../../tests/errno_kinds.rs) |
| [CLI](../../src/cli/ntfs/mod.rs), [harness commands](../../src/bin/rust_ntfs/main.rs) | §7 | Command dispatch into the same primitives; no distinct on-disk format | [CLI verbs](../../tests/cli_verbs.rs), installed CLI shell tests |
| [test_scratch.rs](../../src/test_scratch.rs) | Testing only | Scratch artifact lifecycle; no NTFS fields | [temporary image policy](../../tests/temp_image_policy.rs) |

Binary assets: `src/upcase-canonical.bin` is the captured casing profile described
in §6; `src/logfile-canonical-12k.bin` is an input transformed by the formatter,
not a byte-identical declaration of the log it currently writes.

## Capability distinctions

| Topic | Reads | Writes/maintenance | Remaining boundary |
| --- | --- | --- | --- |
| Ordinary data | Resident/nonresident, ranged | Rewrite, promote, grow, shrink | Not all helpers accept every representation |
| Sparse data | Holes and VDL zeros | Whole-file sparse replacement | In-place writes do not allocate holes |
| LZNT1 | Unit decoding | No compression encoder | Malformed unit topology needs broader oracle checks |
| WOF/EFS | Metadata accessible as supported | No transformed content writes | WOF unnamed data and EFS content refused |
| ADS | Named values, supported extensions | Resident/nonresident | No automatic attribute overflow emission |
| Attribute lists | Single VCN-zero extension | No overflow writer | Multi-segment streams refused; identity checks incomplete |
| MFT growth | Run-based records | Batch growth, bitmap and mirror updates | Record crossing extent boundary; base-record capacity |
| Large directories | Routed lookup, allocated-block listing | Promotion, bounded recursive splits/insertion | Bitmap growth, interior deletion, rebalancing |
| EAs | Resident | Resident payload and summary | Nonresident and full corruption validation |
| Reparse/object IDs | Supported local payloads | Local attribute mutation | Related global indexes not maintained |
| Security | ID/descriptor bytes as exposed | Formatter catalog and ID retargeting | ACL evaluation/inheritance and runtime catalog growth |
| Recovery | Restart classification | Captured LFS 2 redo/undo | Other versions/opcodes, partial application crash |
| fsck | Narrow checks/report | Replay/flag repair | No complete structural/ownership repair |
| Case sensitivity | Default upcase collation | Same default collation | Ordinal helper not wired to directory policy |

## Independent Windows evidence

The retained [2026-10-10 full matrix](../testing/windows-full-matrix-2026-10-10/README.md)
reports 72/72 scenarios and 438 completed recipe steps on Linux ARM64 / Windows
ARM64. Its driver binaries came from revision
`fe450f1798993cf1c8d7b4866a0855761d9aa5c6`; the test branch and harness revisions
and hashes are in the report. This is historical evidence, not a new run of the
source audited here and not per-field verification of every table above.

Four cases did not complete an online `/scan`; they passed the recorded offline
`/F /X` fallback. The matrix command passed, but initially exceeded the output
budget; the report distinguishes that exit 65 from the subsequent budget replay.
The [targeted insertion](../testing/windows-index-insertion-2026-10-10/README.md)
separately records successful enumeration and actual scans for its one case.

## Validation of this documentation revision

`chore test:suite` was attempted before and after edits. Both attempts failed at
fixture validation without running the Rust suite. Missing:

```
ntfs-basic.img ntfs-manyfiles.img ntfs-large-file.img ntfs-sparse.img
ntfs-ads.img ntfs-unicode.img ntfs-deep.img ntfs-attrlist.img
ntfs-compressed.img ntfs-symlink.img ntfs-symlink.targets.tsv
```

The gate names `test-disks/build-ntfs-feature-images.sh` for portable fixtures and
`test-disks/build-windows-native-read-fixtures.ps1` for Windows-only fixtures.
No fixture test is counted as passing or silently skipped. The overloaded four-CPU
host was not used to manufacture fixtures or start a Windows run for a change that
only edits Markdown. Source/test paths, local links/anchors, layouts against named
functions, and whitespace are the applicable documentation checks.

Coverage means the named module's concepts were reconciled into the reference;
it does not mean every malformed-input path, historical format variation, or
Windows kernel policy has been independently reconstructed. Unresolved items have
checkable experiments in [the question register](notes/open-questions.md).
