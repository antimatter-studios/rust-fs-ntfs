# Named-stream quota charges measured on Windows

## Failure and oracle

The named-stream scenario in [CI run 41508136065](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/41508136065),
at `b5ed6e375d2d9ffdd1f842846b1941317edbc3d4`, passed both chkdsk lanes but failed
the quota recount: the Administrators owner held 134144 bytes while Windows
counted 137216, a difference of 3072 bytes. The separate
`validate rust-ntfs format (Windows chkdsk)` job aggregated that Windows job's
failure; it did not report another filesystem defect. The full integration
suite in that run passed.

The oracle measurements below were taken on November 2, 2026, with Windows
ARM64 reporting version `10.0.26100.0` and `ntfs.sys` file version
`10.0.26100.1 (WinBuild.160101.0800)`. Windows formatted the volume, wrote
`plain.txt`, enabled quota tracking with `fsutil quota track`, and created
`win-streams.txt` with two 70000-byte named streams using the existing
`win-format`, `win-write-content`, `win-quota-track`, and `win-write-streams`
matrix operations. The cluster size was 4096 bytes and the file-record size
was 1024 bytes.

Each subsequent operation was performed by Windows, not this crate. Named
streams were overwritten with `CreateFileW` (`CREATE_ALWAYS`) and `WriteFile`;
`Remove-Item -LiteralPath <file> -Stream <name>` deleted them. A disk-image
detach after each operation flushed its state, using the existing `_lib.ps1`
`Dismount-VhdAndCleanup` / `Sync-VhdToImg` helpers. Each resulting raw image was
read independently of the production quota calculator, following the boot
sector, MFT runs, update-sequence fixups, and attribute headers, as the raw
reader in `tests/quota_charge.rs` does.

The compared fields were the quota charge at offset `0x38` of the resident
`$STANDARD_INFORMATION` value, each `$DATA` attribute's name and resident flag,
and the allocated size at offset `0x28` of each non-resident first extent
(`LowestVcn == 0` at offset `0x10`). These are on-disk measurements, not an
interpretation of an API buffer as an MFT record.

[Microsoft's `FILE_QUOTA_INFORMATION` documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_quota_information)
defines `QuotaUsed` as the user's current disk-space usage on the volume. It
does not specify the per-stream accounting formula below; that rule is
established by these Windows-generated records and Windows' own recount.

## Windows measurements

Each file in this table has a resident unnamed stream. The last column is
the file's charge minus its non-resident data allocation, not extra physical
MFT records allocated to the file. All attributes fit in the base record.

| Windows operation and file | Named streams left | Non-resident allocation | File charge | Record-sized quota amounts |
| --- | --- | ---: | ---: | ---: |
| Write `win-streams.txt:s1` and `:s2`, 70000 bytes each | two non-resident | 147456 | 150528 | 3 × 1024 |
| Rewrite both to 20000 bytes | two non-resident | 40960 | 44032 | 3 × 1024 |
| Delete `win-streams.txt:s2` | one non-resident | 20480 | 22528 | 2 × 1024 |
| Give `plain.txt` two 20000-byte streams | two non-resident | 40960 | 44032 | 3 × 1024 |
| Rewrite `plain.txt:s1` to 100 bytes and `:s2` to zero | two still non-resident | 4096 | 7168 | 3 × 1024 |
| Delete the empty non-resident `plain.txt:s2` | one non-resident | 4096 | 6144 | 2 × 1024 |
| Create `resident.txt` with fresh 100-byte and empty named streams | two resident | 0 | 3072 | 3 × 1024 |
| Delete the 100-byte resident stream, leaving the empty one | one resident | 0 | 2048 | 2 × 1024 |

A second Windows-formatted volume used `Format-Volume -UseLargeFRS` with
4096-byte clusters. `fsutil fsinfo ntfsinfo` reported 4096-byte file records.
Its `plain.txt` held a resident unnamed stream, one 20000-byte non-resident
named stream, and two resident named streams of 100 and zero bytes. Windows
charged **36864 bytes**: 20480 allocated bytes plus **4 × 4096**, confirming
that the per-stream amount follows the file-record size rather than a fixed
1024-byte constant.

For these base-record-only files, the measured rule is:

```text
file quota charge = file-record size × (1 + number of named $DATA streams)
                  + allocated size of all non-resident $DATA streams
```

Resident and empty named streams still contribute their record-sized amount.
A continuation extent is not another stream. Files with attribute lists,
sparse streams, and compressed streams remain separate, explicitly excluded
accounting shapes (#852, #853, #854).

## Exact difference in the failed scenario

Applying the unchanged scenario's two `mac-write-streams` operations to the
Windows-made initial image with the old binary gave:

| File after the scenario's writes and delete | Old crate charge | Windows charge for the same stream lengths | Missing |
| --- | ---: | ---: | ---: |
| `plain.txt`, two 20000-byte named streams | 41984 | 44032 | 2048 |
| `win-streams.txt`, one 20000-byte named stream | 21504 | 22528 | 1024 |
| Total | 63488 | 66560 | 3072 |

The missing three record-sized stream charges explain the whole CI recount
difference. Chkdsk alone did not detect it.

## Regression and correction

The original nine quota tests passed because their expected charge was the
same allocation-only formula used by the writer. The added non-resident
regression on the existing Windows-made tracked fixture failed with 91136
bytes charged instead of 92160. The added resident-stream regression failed
with 1024 instead of 2048 immediately after creating its first named stream
through the resident-only API.

`quota_charge_due` now includes each named stream's record-sized amount and
only reads a non-resident stream's allocation from its first extent. The
resident-only writer also invokes the same recharge operation; allocating
no clusters does not mean that a new stream has no quota cost. The existing
write/shrink/delete assertion now includes this measured stream charge
instead of repeating the old allocation-only assumption. The new tests also
check the owner's aggregate usage, replacement without double charging,
deletion of a resident stream, and refund of the last empty stream.

## Validation receipt

The unchanged `win-format-win-quota-mac-streams-win-chkdsk` scenario passed
on the local Windows ARM64 VM on November 2, 2026, from 21:27:21 to 21:36:40
UTC. Step 10 ran `win-chkdsk` with `readonly,/scan` and a `Clean` verdict,
returning zero. Step 11 returned zero and printed:

```text
win-quota-verify: every owner's usage is what Windows counts (3 owners)
```

The tested release binary's SHA-256 was
`27e11820a5c565a8affb7deba9c75e297f6995fa9362b1c50671203b8026934b`.
The unchanged `test-matrix.json` SHA-256 was
`0a196e567025184a64427ed27ee2edd451ed88bc04ad24c31d482c0c8fd1c528`.
The local Windows harness reported `v4.5.0-10-g8cbde9e`; CI uses the
repository's declared `v4.4.0` pin. The per-step exit receipts and quota
verification output were retained under
`test-diagnostics/matrix/win-format-win-quota-mac-streams-win-chkdsk/`.

All 11 quota regressions passed. The local full-suite comparison preserved
all 1567 baseline passing cases and added the two new passing regressions.
Both runs had the same 382 failing cases, caused by missing local fixtures
and test tools; the local full suite is not green. Formatting, clippy, source
policy, and the agent-core guard from the declared `rust-fs-core v0.3.14`
pin passed. The combined local `chore lint` stopped because the separately
checked-out core guard expects agent-core v8 while this repository's
unchanged shared block is v7.
