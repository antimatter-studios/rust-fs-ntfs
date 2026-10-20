# Windows format geometry capture

Issue #887 is the capture slice of #699 and #588. The `Windows format
geometry capture` workflow runs on an elevated hosted `windows-latest`
runner. It records 26 requests: allocation units 512 B through 2 MiB in
powers of two, each with `UseLargeFRS` false and true. The latter is the
PowerShell equivalent of `format /L`.

The collector creates its own dynamic 1 GiB VHD, verifies that Windows
reports 512-byte logical sectors, and creates one GPT partition using the
remaining disk space. Every request uses `Format-Volume -FileSystem NTFS
-AllocationUnitSize <bytes> -UseLargeFRS:<bool> -NewFileSystemLabel GEOMETRY
-Force -Confirm:$false`.
The exact invocation and partition size are retained, so acceptance is an
observation at that size, not a claim about every volume size.

`measurements.json` records successful requests and native CIM formatter
errors. A successful request includes bytes per sector, the three raw BPB
encoding bytes, decoded cluster/record/index sizes, and both MFT LCNs.
`fsutil fsinfo ntfsinfo` independently corroborates the sector, cluster and
record sizes and the MFT/MFTMirr locations. Its complete text is retained.
Missing or duplicate native fields, disagreements, raw read errors and
storage setup errors fail the capture. Both 4 KiB control formats must
succeed, and the final request set must contain exactly 26 unique rows.

`provenance.json` records the source revision, runner image and image
version, Windows caption/version/build/update revision, capture time, disk and partition
sizes and the commands used. Only JSON and text are uploaded; the scratch
VHD is detached and removed in `finally`, including after failures. Partial
rows remain diagnostic evidence, never a completed table.

For the 64 KiB control and the five large cluster sizes (128 KiB to 2 MiB),
each without large records, issue #892 also records where Windows' own
formatter put `$MFTMirr`, `$Secure:$SDS` and the root directory's
`$INDEX_ALLOCATION:$I30`: the starting LCN, the allocated cluster count and
every extent, in `large-cluster-layout.json`. The values come from
`FSCTL_GET_RETRIEVAL_POINTERS`, answered by the Windows filesystem driver,
not from this crate's parser. An unreadable file, an unallocated or
discontinuous extent, a refused request or a missing size fails the capture,
naming the file and cluster size. #700's formatter work (#893) lays these
files out against those measurements instead of extrapolating 64 KiB.

Issue #896 adds Windows' own NTFS format of a disk whose logical sector
size is 4096 bytes (4Kn), the reference #701's formatter work grades against.
A dynamic VHD only exposes 512-byte sectors, so the collector tries, in
order, `New-VHD -LogicalSectorSizeBytes 4096 -PhysicalSectorSizeBytes 4096`
(present only where the Hyper-V module is), then a diskpart VHDX whose two
sector size metadata items are set to 4096. Every attempt and its diagnostic
is kept in `sector-4096.json` (or `sector-4096-attempts.json` when none
worked), and `Get-Disk` on the attached image must report 4096-byte logical
sectors before anything is formatted, so a 512-byte disk cannot pass as 4Kn.
The volume is made by `Format-Volume` at its default allocation unit. The
row records `Get-Disk`'s logical and physical sector sizes; `fsutil`'s bytes
per sector, physical sector, cluster and file record and both MFT LCNs; the
4096-byte boot sector's raw fields and `NumberSectors`; which of the two
documented backup locations (`NumberSectors` sectors in, and the partition's
last sector) hold a copy of the boot sector; and the update sequence offset
and count of `$MFT` record 0 and of the root directory's first index block,
read raw through `PhysicalDrive`, from which the stride Windows uses on 4Kn
follows. Only those fixed header fields are read: the stride is measured,
not assumed. `sector-4096-provenance.json` and `sector-4096-ntfsinfo.txt`
are kept beside the row; the scratch VHDX is detached and removed in
`finally`.

Issue #901 adds Windows' own NTFS format of a 512e disk (512-byte logical,
4096-byte physical sectors) beside a 512n disk of the same 1 GiB size, the
reference #702's planner work (#902) grades against. NTFS records only the
logical sector size, so whether the physical size changes Windows' layout is
measured, not assumed. The same collector makes each disk with the logical
and physical sizes set separately, and `Get-Disk` must report 512 and 4096
(512e) or 512 and 512 (512n) before anything is formatted, so a mislabelled
disk cannot pass. Each row (`sector-512.json`, `sector-512-4096.json`) carries
the 4Kn row's fields plus the partition offset. Windows denies opening its
live `$LogFile` metadata file for extent queries, so its layout is recorded as
unavailable in the provenance and is not part of this comparison.
`sector-512e-diff.json`
then names every compared field where the two formats differ; the physical
sector size itself is the variable under test and is not compared. An empty
list is a valid, recorded result: it means Windows' layout does not depend on
the physical sector size.

This slice deliberately does not supply invented measurements. #888 adds
native `$LogFile` size, #889 measures maximum cluster-count boundaries,
and #890 retains the measured table under this directory, checks for
regeneration drift and grades `plan_format`. No planner parity or largest
supported volume claim is made by the capture workflow.

The 59 fixture-free PowerShell contracts cover request coverage, signed
encoding boundaries, malformed BPBs, native field parsing, disagreement,
duplicates, missing rows, the control formats, extent decoding, complete
large-cluster layout rows, 4096-byte boot sectors, update sequence headers,
complete 4Kn rows from a disk that reports 4096-byte sectors, complete 512e
and 512n rows from disks that report their physical size, and the 512e diff
naming each changed field. These synthetic bytes
test collector mechanics; Microsoft's formatter supplies the actual oracle.

Sources: Microsoft's [Format-Volume documentation](https://learn.microsoft.com/en-us/powershell/module/storage/format-volume)
and [format command documentation](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/format).
No third-party NTFS implementation source is used.
