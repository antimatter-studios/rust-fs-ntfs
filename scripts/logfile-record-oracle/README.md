# Bounded checkpoint header oracle

This test-only executable makes **eight comparisons** per input: seven
checkpoint fields (`RecordView.record_type`, `client_previous_lsn`,
`client_undo_next_lsn`, `seq_number`, `client_index`, `transaction_id` and
`flags`) and the current record's `client_data_length`. It does not run
replay. The independent reader is
[Sootmark/ntfslog at b885856](https://github.com/Sootmark/ntfslog/tree/b88585646801ee2c1e2acf91a078da6ab57af778),
pinned in Cargo.toml and Cargo.lock. Its package metadata and LICENSE-MIT
were checked before inspecting its public API: MIT OR Apache-2.0. No upstream
implementation or fixture bytes are copied into this repository.

The published [LFS_RECORD format table](https://flatcap.github.io/linux-ntfs/ntfs/files/logfile.html)
assigns ClientRestart the record type 2. The expected type is used only after
the independent decoder returns `Body::Checkpoint` at the NTFS client's
restart LSN. A missing restart/client/record, ambiguous identity, unexpected
body, empty/oversized input or differing field fails. There is no fallback
to our parser for expected evidence and no record is skipped on disagreement.

The two link values come directly from the pinned decoder's public
`LogRecord.previous_lsn` and `LogRecord.undo_next_lsn` fields for that same
independently selected checkpoint. No raw-field decoder is defined here and
no expected value or record location comes from `RecordView`. Zero links are
compared too; this does not prove nonzero-link traversal or update records.

The client sequence and index come directly from the same independent
`LogRecord.client.sequence` and `LogRecord.client.index` fields. They are
compared with `RecordView.seq_number` and `RecordView.client_index`, including
zero values. Transaction ID and all flag bits come directly from
`LogRecord.transaction_id` and `LogRecord.flags` for that checkpoint.

The pinned public `LogRecord` does not expose client-data length. Instead,
the independently parsed `RestartArea.last_lsn_data_length` grades the record
identified by that same area's `current_lsn`. The helper requires exactly one
independently decoded record at that LSN before asking `RecordView` for it.
Missing or ambiguous current records fail. No length is inferred from a
checkpoint's decoded members, a raw-header read or our decoder. The current
record and checkpoint can differ; each TSV row carries its own selected LSN.
This does not establish the checkpoint's length when their LSNs differ.
The helper prints all eight expected/actual rows before refusing a mismatch,
so a failing comparison retains all available field evidence.

The existing integration CI job runs `scripts/compare-logfile-record-type.sh`
after downloading Windows-authored fixtures. It checks immutable interrupted
capture 1, SHA-256 `e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb`,
and one mounted Windows log, for sixteen comparisons across both inputs.
`build-windows-logfile-checkpoints.ps1` reuses the native VSS producer with
three candidates, a 24-second workload and 128 MiB volumes. Windows reports
each snapshot/image digest and partition bounds. Linux verifies those digests
before external `ntfscat -f -i 2` extraction.

The merge gate reads pinned captures. A snapshot of a volume Windows is still
writing holds whatever the timing gave it: ntfs-3g refused the first one in
four of five merge-group runs, and a fresh capture can lack a record of the
shape a comparison selects, so a live capture made the gate fail at random.
The four `$LogFile` extracts of merge-group run 38923190267 are committed as
`test-disks/logfile-checkpoint-{1..4}.logfile.gz`, with each extract's size and
SHA-256 and its Windows image's digest in
`test-disks/logfile-checkpoint-captures.json`; both comparison scripts refuse
an extract that differs from it. `LOGFILE_CHECKPOINTS=live` makes them read
the run's own images instead, which
`.github/workflows/logfile-checkpoint-drift.yml` does on a daily schedule. A
red run there means Windows, ntfs-3g or the producer moved; a better fresh
capture is pinned from that run's artifact.

`--select-checkpoint` runs only the independent decoder: it requires LFS 2.x,
one NTFS client, exactly one checkpoint at that client's restart LSN, and
exactly one decoded current record. It retains the input SHA-256, decoder
revision, version and both LSNs. The first independently eligible candidate
is selected; our parser never chooses a replacement after disagreement.
All three failing candidates make the job fail, with extraction/decoder
diagnostics retained. The selected raw log's digest is checked again before
comparison. `checkpoint_identity.rs` independently asserts capture 1's
version/checkpoint identity (`0x33e791`), retains source immutability, and
requires the original hashed LFS1 capture to fail selection. The unit
checkpoint regression uses that same immutable LFS2 input and retains its
positive identity assertion; a separate unit control requires LFS1 refusal.

`--checkpoint-only` still requires all eight original comparisons. It bounds
this metadata experiment separately from the unchanged dedicated nonzero-link
regression, which uses the default helper mode and still requires both links
of a live nonzero record (see "Live-log selection" below). No successful
nonzero comparison is inferred from this mode.
The TSV artifacts retain the exact input
digest, oracle revision, chosen LSN, field name, expected and actual values, including
on a comparison failure. Input is limited to 64 MiB. No images or tool bytes
are committed. Before this producer/selection repair, the captured/fresh path was blocked: the
completed integration job in [run 38765203671](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38765203671/job/116535468276)
refused the clean capture as LFS version 1 before emitting any comparisons.
Its [artifact 11989348470](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38765203671/artifacts/11989348470)
contains an empty `captured.tsv`; it proves no field agreement. The LFS 2.x
parser restriction is unchanged.

The repair has not yet run in CI. Supported fresh input, sixteen comparison
rows and a green checkpoint regression remain requirements, not claimed results.
The terminal baseline at `dfc7733`, run 38771033869, reports 724 passed and
one failure in `windows_checkpoint_record_matches_its_restart_lsn`, with the
same LFS1 refusal. Original captures, strict lookup and parser limits remain.
`test-disks/logfile-record-checkpoint.tsv` binds the supported positive
integration assertion, independent identity control and eight-field captured
comparison to the same immutable capture, SHA-256, decoder revision, LFS major
version and checkpoint LSN. The selector must agree with the manifest before
comparison. The original second interrupted capture's positive identity and
immutability assertions remain. The original clean LFS1 capture now has a
separate integration test requiring the specific version refusal and source
immutability; all three captures retain the invalid-LSN and budget controls.
Missing captures fail explicitly and name their producer. This wiring has not
yet been executed in CI and does not supply the outstanding sixteen-row proof.

A separate CI regression uses the existing, immutable Windows interrupted
capture 1, SHA-256
`e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb`.
The pinned decoder independently selects the first record with either link
nonzero; no location or expected link comes from RecordView. Both links are
then compared in addition to the original eight fields, with ambiguity,
capture hash and source-immutability checks retained. Absence of a nonzero
record fails the regression. This proves link field values when green,
without following either link or grading update operations.

The test-only commit `92b44ce` was red in completed
[run 38770428308](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38770428308)
before the helper changed: the independent decoder found record `0x1046bb`
with previous and undo-next links both `0x10443c`, while the report contained
only the zero-link checkpoint. Its retained `logfile-nonzero-links`
[artifact 11992085350](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38770428308/artifacts/11992085350)
contains every independently decoded link, decoder problems and the original
eight comparison rows. The unchanged regression must turn green in CI before
nonzero RecordView agreement is claimed. Oracle executions remain CI-only.

The follow-up at `6c9fc36` completed with failure in
[run 38770739379](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38770739379).
Strict RecordView lookup refuses the independently decoded record:
`the record at LSN 0x1046bb is not where it says`.
[Artifact 11990988216](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38770739379/artifacts/11990988216)
retains the independent links, all eight successful existing comparisons
and the helper refusal. Thus a Windows-authored nonzero record exists,
but this selected record does not establish RecordView agreement. No claim
is made that every other record or capture is unsuitable. The regression
remains red; resolving strict lookup or independently selecting another
eligible immutable record requires a further CI experiment. No bytes were
fabricated and no parser restriction was relaxed.

`physical_position.rs` adds a separate CI-only experiment for those exact
Windows records, `0x1046bb` and its previous/undo-next `0x10443c`. It requires
both independent identities, retains the pinned reader's physical offsets,
and checks their first three raw header words against the independent LSN
and links. A sector-trailer crossing fails this byte control rather than
silently substituting unprotected or production-decoded bytes. The immutable
capture digest and source immutability remain mandatory.

The `logfile-physical-position` artifact contains `positions.tsv`, every
aligned raw LSN byte hit (including possible payload hits), raw hit/addressed
pages, the full independent decode/problems and both strict lookup results.
The LSN byte address uses only the independently decoded restart area's
sequence-bit count; no expected offset comes from production. A refusal is
retained as evidence and does not satisfy or replace the existing positive
nonzero-link regression. Physical/address disagreement alone does not settle
copy freshness, traversal or recovery eligibility. This experiment has not
yet executed in CI; its retained Windows bytes must decide the discrepancy.

Run in CI from the repository root:

```sh
bash scripts/compare-logfile-record-type.sh
```

The fresh pair is selected from the live log only: an update record with both
links nonzero, whose source and both targets are no older than the NTFS
client's oldest LSN. An older record survives only as a stale copy from an
earlier pass of the circular log, away from the offset its LSN names, and a
one-link source cannot satisfy the two-edge physical proof.

The existing fresh-pair validation also binds each independently selected
source and target to its complete physical record. It validates every page
USA trailer before restoring the original sector-end words, requires a
complete single-page update, and retains the complete protected and restored
record hex plus both SHA-256 digests in `fresh-pair-physical.tsv`. Exactly one
complete protected byte sequence must occur in the capture, at the independent
decoder's offset. All exposed header fields, operation codes, declared payload
lengths and actual redo/undo payload bytes must agree with the independent
record. Strict lookup must return the view parsed from those complete bytes
for the source and for both nonzero source links. Both the adjacent physical
position and adjacent LSN are rejection controls. No candidate is substituted
after a production disagreement.
Every physical row is retained before lookup grading. `fresh-pair-strict.tsv`
records the expected view, lookup result and adjacent-LSN rejection for all
three roles, even when the source lookup fails.

Historical full run `38819608901`, artifact `12011549770`, independently
selected source `0x104c22` at `0x16110` and both links to `0x104c0d` at
`0x16068`, using decoder `b88585646801ee2c1e2acf91a078da6ab57af778`.
The selected `candidate-2.bin` SHA-256 is
`4d8adbd983ab4683d27390c1f1337d1c5906e6c4ca87d6da2dd2b2eb24acb338`.
For that capture, the regression additionally pins the exact identities,
offsets, client-data lengths (392 and 120), and complete raw-record hashes
(`994f177d68b660bba58347b0ff09dc23771d2ff3d1506e18a4e31807e70ee85a`
and `65c31801e07d4a036f2113da9ee0b98ff5098b73aab1073637864e54f841a879`).
These hashes were measured directly from the archived physical bytes, not
from RecordView. To reproduce that exact pair in CI, extract that archive's
`candidate-2.bin` and `fresh-complete-pair.tsv`, then pass their absolute paths
as `LOGFILE_PAIR_RAW` and `LOGFILE_PAIR_REPORT`, with the pinned capture digest
as `LOGFILE_PAIR_SHA`, to the binary test
`fresh_pair_report_matches_genuine_independent_records`. The normal existing
CI route runs the same regression on its frozen fresh Windows selection.
Archived and new-capture execution of these added physical/traversal assertions
is still owed. The prior full run failed on a relative-path read before this
validation; inherited commit `36e0835` fixes that path and remains intact.

This fresh-pair proof does not repair the immutable interrupted-capture red:
run `38819591816`, job `116712632500`, refuses `0x1046bb`; the independent
physical test still requires exactly one decoded `0x10443c` and finds zero.
The original capture digest, identities and assertions remain unchanged.

Round 19 retains the exact completed Ubuntu red at `f692b77`:
[run 38834346227, job 116760027915](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38834346227/job/116760027915).
Its [physical artifact 12018493851](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38834346227/artifacts/12018493851)
places the sole independently decoded `0x1046bb` at `0x135d8`, with both
links to `0x10443c`. The target's only aligned raw hits are `0x135e0` and
`0x135e8`, the source's previous/undo pointer words. Neither is an independently
decoded target header. The immutable gate still requires exactly one target;
pointer occurrences do not satisfy it. The separate immutable nonzero gate
also completed red in [run 38834346110, job 116759976745](https://github.com/antimatter-studios/rust-fs-ntfs/actions/runs/38834346110/job/116759976745)
with the unchanged source-position refusal.

`raw_location.rs` runs in Ubuntu CI even after the original gate fails, before
the same always-run artifact upload. It retains `raw-location-roles.tsv`,
mapping every aligned source/target hit to independently decoded header and
pointer-owner offsets. `raw-location-controls.tsv` records rejection of
pointer positions, wrong LSN, truncated header/page, wrong page magic/USA
sequence/count, and each of eight torn sector trailers. It validates all
captured USA trailers and retains the complete protected/restored source
record plus restored page. The source's 88-byte client length and full
protected-record hash are pinned to direct archived-byte measurements.
These diagnostic controls do not assert that target absence is correct,
establish target traversal, or replace either original positive gate. Native
evidence for the original target remains missing. No image, LSN, decoder pin,
payload assertion, writer behavior or fresh-pair requirement is changed.

Round 20 adds a separate CI entry point, `bash scripts/compare-logfile-native-metadata.sh`,
which runs even when the original comparison step fails. The original eight
rows, both nonzero links, physical/USA/payload controls and immutable tests
are unchanged. `--select-metadata` fixes at most eight unique independent
record identities before production grading, requiring type variation and
at least one other varying metadata field. It retains witnesses for every
observed varying field; constant and zero fields remain valid observations,
not claims of variation. `--compare-metadata` grades all seven exposed header
fields on those same records, preserving every row even after lookup refusal.
Selector controls reject absent/type-only variation and ambiguous identities;
field controls reject a wrong value in each of the seven columns and accept
zero observations. Real-input wrong-digest controls must fail by name.

`--select-checkpoint-length` requires one independently decoded NTFS checkpoint
whose client restart LSN equals the independent restart area's current LSN.
Only then can `last_lsn_data_length` attest checkpoint client-data length.
`--compare-checkpoint-length` explicitly grades that checkpoint length, plus
its seven header fields. Distinct current/checkpoint identities fail the named
precondition; no checkpoint length is inferred from RecordView or raw offsets.
The native producer additionally ends its workload, flushes the still-mounted
inner volume with Windows FlushFileBuffers and captures a fourth VSS image.
The original three candidates and their selection order remain intact. This
probe is an experiment, not a claim that Windows must produce equal LSNs.
Windows image hashes, bounds and probe operation provenance are retained;
Linux verifies all four images before external extraction. Missing native
variation or identity evidence fails, with selection diagnostics retained.
Each route freezes the first independently eligible input, rechecks its hash
and selection report, and never substitutes another after production refusal.
CI retains `native-*-selection.tsv`, `native-*-comparison.tsv`, raw inputs and
stderr in the existing `logfile-record-type-oracle` artifact. Actual native
variation, checkpoint-length agreement and full-suite green remain owed.

Native extraction has its own fail-closed contract. Every fixed candidate is
recorded in `native-extraction.tsv` with tool exit status, output byte count
and diagnostic path. A refusal, empty output or output over 64 MiB, including
a tool refusal after partial output, is named on the console and that
candidate supplies no selection; it never decodes refused bytes. The step
stays red when a route finds no readable candidate to select. ntfs-3g refused
the first snapshot with the missing-`$Secure` error in four of five
merge-group runs (38925071276, 38933834125, 38933837162, 38933840377), a
snapshot taken before NTFS had written `$Secure` out, so a refused candidate
alone does not fail the step. Tool errors
are named on the console with at most 2048 bytes of detail, and complete stderr
remains in the artifact. CI transport controls exercise a valid extraction,
the observed missing-`$Secure` refusal, partial-output refusal and empty success,
including first-failure retention while later evidence is collected. Those
stand-in controls attest the transport contract only, never native agreement.

Full-field independent decoding remains unmet. This comparison does not
grade operation codes, redo/undo offsets or payload lengths. Checkpoint
comparisons cover header type, links, client identity, transaction ID and
flags; client-data length is graded only at the current record. Update
operation decoding and nonzero-link traversal remain unproved. This does
not establish recovery eligibility or resolve conflicting page copies.

## Live-log selection

The nonzero-link record and its link target are chosen from the live log
only: a record, and every record it links to, no older than the independent
restart area's NTFS client `oldest_lsn`. Interrupted capture 1 is a 2 MiB log
with 45 sequence bits, so it wraps every `0x80000` LSNs; its client's oldest
LSN is `0x33e786` (pass 6). The record first selected, `0x1046bb`, and its
target `0x10443c` are from pass 2. `0x1046bb` survives only in a stale
tail-buffer page (file offset `0x13000`, a pass-2 copy of home page `0x23000`,
whose live copy is pass 6), and `0x10443c` is in no page at all: its home page
`0x22000` was overwritten by later passes, and its only raw occurrences are
`0x1046bb`'s link words. Strict lookup refuses the first and no decoder can
find the second, so neither could ever satisfy the gates.

The helper's default mode, `nonzero_links.rs` and `physical_position.rs` now
use the first live record with a nonzero link whose targets are live too:
`0x33e7c6` (transaction 24, `SetNewAttributeSizes`) at `0x1f3e30`, with
previous and undo-next links both `0x33e7a5` (transaction 24,
`OpenNonresidentAttribute`) at `0x1f3d28`. Every assertion is retained for
the live pair: exactly one independent match, a nonzero link, exactly one
raw header copy at the decoder's own offset, no sector-trailer crossing, the
wrong-position control and the pinned links. `nonzero_links.rs` also requires
each nonzero link target to be decoded exactly once. `raw_location.rs` keeps
its pass-2 diagnostic, which grades raw pointer and torn-page controls, not
record identity.
