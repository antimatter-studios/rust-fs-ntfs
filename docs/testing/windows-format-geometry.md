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

This slice deliberately does not supply invented measurements. #888 adds
native `$LogFile` size, #889 measures maximum cluster-count boundaries,
and #890 retains the measured table under this directory, checks for
regeneration drift and grades `plan_format`. No planner parity or largest
supported volume claim is made by the capture workflow.

The 33 fixture-free PowerShell contracts cover request coverage, signed
encoding boundaries, malformed BPBs, native field parsing, disagreement,
duplicates, missing rows, the control formats, extent decoding and complete
large-cluster layout rows. These synthetic bytes
test collector mechanics; Microsoft's formatter supplies the actual oracle.

Sources: Microsoft's [Format-Volume documentation](https://learn.microsoft.com/en-us/powershell/module/storage/format-volume)
and [format command documentation](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/format).
No third-party NTFS implementation source is used.
