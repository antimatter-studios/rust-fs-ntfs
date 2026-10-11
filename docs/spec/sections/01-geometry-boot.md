[TOC](../ntfs-specification.md) | [Next: MFT records](02-mft-records.md)

# 1. Volume geometry and boot sector

<a id="overview"></a>

## Overview

An image presented to this driver begins at the NTFS volume, not at a partition
table. Volume byte zero contains the boot record. The boot fields locate the MFT
bootstrap and describe addressing geometry. Subsequent metadata locates itself
through attributes and run lists. Partition offsets belong to the device layer;
adding `HiddenSectors` again to an already sliced volume would misaddress it.
**IMPLEMENTED:** [block_io.rs](../../../src/block_io.rs),
[mft_io.rs](../../../src/mft_io.rs) `read_boot_params_io`.

<a id="boot-sector-layout"></a>

## Boot sector layout

The formatter constructs a 512-byte boot prefix and writes its backup at the final
sector of the formatted cluster-aligned extent. This table records fields emitted by `build_boot_sector` in
[mkfs.rs](../../../src/mkfs.rs); constants and zeros describe this formatter's
profile, not every historical NTFS boot record. **IMPLEMENTED.**

| Offset | Bytes | Field | Meaning or emitted value |
| --- | --- | --- | --- |
| `00` | 3 | Jump | `EB 52 90` |
| `03` | 8 | OEM | ASCII `NTFS    ` |
| `0B` | 2 | Bytes per sector | Logical-sector geometry |
| `0D` | 1 | Sectors per cluster | Literal count or exponent encoding below |
| `0E` | 2 | Reserved sectors | Zero |
| `10` | 1 | Legacy FAT count | Zero |
| `11` | 2 | Legacy root-entry count | Zero |
| `13` | 2 | Legacy short sector count | Zero |
| `15` | 1 | Media descriptor | `F8` |
| `16` | 2 | Legacy FAT size | Zero |
| `18` | 2 | Sectors per track | 63 in this formatter, not used by addressing |
| `1A` | 2 | Heads | 255 in this formatter, not used by addressing |
| `1C` | 4 | Hidden sectors | Zero for this volume-relative formatter |
| `20` | 4 | Legacy long sector count | Zero |
| `24` | 4 | Drive/extended BPB bytes | `00800080` in this formatter |
| `28` | 8 | Total sectors | Formatter excludes the final backup sector |
| `30` | 8 | MFT starting LCN | Bootstrap record 0 |
| `38` | 8 | MFT mirror starting LCN | Backup of initial records |
| `40` | 1 | FILE record size | Signed cluster count or byte exponent |
| `41` | 3 | Reserved | Zero |
| `44` | 1 | Index block size | Same size encoding as `40` |
| `45` | 3 | Reserved | Zero |
| `48` | 8 | Serial number | Volume identifier |
| `50` | 4 | Checksum field | Not validated by this driver |
| `54` | 426 | Boot code area | Minimal stub, not a Windows bootloader |
| `1FE` | 2 | Boot marker | Bytes `55 AA` |

The earlier draft misplaced the legacy root-entry field at `13`; it is at `11`.
Unused historical fields do not provide alternate locations for NTFS geometry.

<a id="log2-encoding"></a>

## Size encodings

For boot bytes `40` and `44`, interpret the byte as signed `i8`:

```
raw > 0: size_bytes = raw * cluster_size
raw < 0: size_bytes = 1 << (-raw)
```

Examples: `F6` is -10 and means 1024 bytes; `F4` is -12 and means
4096 bytes. At 512-byte clusters, an index-size byte of `08` means 4096 bytes.
At 4096-byte clusters, `01` means the same block size. The FILE-size parser
rejects zero and out-of-range sizes. The index-size parser instead falls back to
one cluster for zero or a negative exponent of 32 or greater; that permissive
fallback is an implementation choice, not evidence that the boot field is valid.
**IMPLEMENTED:** `parse_boot_params_from_bytes`, `build_boot_sector`.

The sectors-per-cluster byte at `0D` has a different boundary. The parser accepts
nonzero power-of-two literal values through `80` inclusive. Bytes above `80`
are interpreted as signed negative exponents of a *sector count*. Thus `80`
is 128 sectors, while `F8` is 256 sectors and `F4` is 4096 sectors. With
512-byte sectors these are 64 KiB, 128 KiB, and 2 MiB clusters respectively.
The parser caps this exponent at 12. This cap is an implementation acceptance
rule; it does not independently establish every Windows geometry limit.
**IMPLEMENTED:** `parse_boot_params_from_bytes`; regression checks in
[boot_sector_geometry.rs](../../../tests/boot_sector_geometry.rs).

<a id="geometry-limits"></a>

## Validation and address bounds

The low-level boot parser reads the first 512 bytes and requires a power-of-two
logical-sector size, a valid cluster-count encoding, and a FILE record size in
512–16384 bytes. It rejects a sector size larger than a FILE record, since no
fixup stride would fit. A record equal to one logical sector is accepted.
The mount/volume-info layer additionally checks the OEM identifier. Do not
attribute boot checksum or backup recovery checks to functions that only parse
geometry. **IMPLEMENTED:** `read_boot_params_io`, `read::read_volume_info`.

The parser intentionally does not reject every image whose declared sector count
exceeds the supplied file length: the source records Windows-created image cases
with this property. Transfers are instead checked against both the declared
volume and the actual device. `cluster_span` uses checked arithmetic for the LCN,
cluster-relative offset, and byte count. Whole-value reads use
`min(declared_volume_bytes, device_bytes)` as an allocation ceiling.
**IMPLEMENTED:** `mft_io::cluster_span`, `read::bounded_value_length`;
[allocation_bounds.rs](../../../tests/allocation_bounds.rs).

These checks do not prove ownership of a mapped range. A run can lie inside the
device and still point at another file. Ownership and protection are separate
[allocation concerns](03-data-runs-bitmap.md#bitmap-update-rules).

<a id="sector-vs-cluster"></a>

## Sector, cluster, record, and stride

```
cluster_size = bytes_per_sector * decoded_sectors_per_cluster
volume_relative_byte = LCN * cluster_size + byte_within_cluster
```

A 512e disk can have 512-byte logical sectors and larger physical sectors. A 4Kn
volume can use 4096-byte logical sectors. The source supports geometry rather than
a universal 512-byte-only rule. Its FILE and INDX fixup calls use the parsed
logical-sector size; LFS log-page fixups use a fixed 512-byte stride. Microsoft's
[multisector notes](https://learn.microsoft.com/en-us/windows/win32/devnotes/multi-sector-header)
describe a 512-byte sequence stride. The relationship between those documented
strides and each Windows 4Kn structure needs fixture-specific validation; it must
not be hidden by equating physical sectors and protection strides.
**DOCUMENTED / IMPLEMENTED / OPEN:** Q01 in the question register.

<a id="boot-mirror-sync"></a>

## Boot file and backup

MFT record 7 describes `$Boot`; its `$DATA` includes more than the first boot
sector. Reserving only the first cluster while placing the MFT inside the rest of
that stream makes live metadata overlap. `format_filesystem` sizes the boot
reservation before placing later metadata. The writer's backup is at
`floor(requested_format_bytes / cluster_size) * cluster_size - bytes_per_sector`; do not replace that with an off-by-one formula
using the formatter's already-shortened `TotalSectors` field.
**IMPLEMENTED:** [mkfs.rs](../../../src/mkfs.rs),
[mft_geometry_layout.rs](../../../tests/boot_sector_geometry.rs).

The driver emits the backup but does not implement general bidirectional boot
repair, partition resize reconciliation, or a mount fallback that arbitrates two
contradictory boot records. Those are **OPEN** compatibility questions.

<a id="ntfs-versions"></a>

## Versions and signatures

The NTFS version lives in `$Volume:$VOLUME_INFORMATION`, not in the OEM string.
The formatter emits 3.1; `read_volume_info` reads major/minor and flags from that
attribute. This does not certify full 1.2 or 3.0 support. The self-record-number
field and standard-information value length need their own version handling.
**IMPLEMENTED:** [read.rs](../../../src/read.rs), [mkfs.rs](../../../src/mkfs.rs).

<a id="signatures"></a>

### Recognized record signatures

| Bytes | Role |
| --- | --- |
| `FILE` | MFT record |
| `INDX` | External index block |
| `RSTR` | Log restart page |
| `RCRD` | Log record page |

The USA update number is a cycling 16-bit word, not a fixed magic number or CRC.
**IMPLEMENTED:** `apply_fixup_on_read_magic` and `apply_fixup_on_write_magic`.

## Open questions

See Q01–Q03 in [the register](../notes/open-questions.md): 4Kn stride evidence,
boot-copy arbitration, and historical version/geometry limits.

[TOC](../ntfs-specification.md) | [Next: MFT records](02-mft-records.md)
