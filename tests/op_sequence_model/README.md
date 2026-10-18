# Seeded operation readback: first slice of #484

`model.rs` is the separate logical tree model from #680, with no dependency
on the driver's readers, writers or record IDs. Its eight fixture-free
contracts live in `tests/op_sequence_reference_model.rs` and run in CI with
their own executed-case floor, as well as in `chore test:suite`. Files share
bytes through model identities when hard-linked; directories retain child
identities across same-directory rename. Refused transitions change nothing.
Names use a supplied 65,536-entry UTF-16 `$UpCase` mapping, not Rust Unicode
case conversion. The 1 MiB file bound is a workload limit, not an NTFS limit.
Directory sizes are modeled as zero, and directory link counts count names,
not POSIX dot entries. Timestamps, open handles, ADS and EAs are outside this
slice. The model is not yet wired into disk execution: #681–#685 cover the
generator, fresh-reopen harness, explorer, regression capture and extensions.
These contracts prove expected-state semantics, not on-disk NTFS correctness;
the existing independent readers below remain necessary for disk validation.

`tests/op_sequence_readback.rs` runs exactly four seeds (0, 1, 484 and
u64::MAX), each with three creates, 120 offset writes/truncates, and one
refused duplicate create. Every accepted operation must complete in full.
The model uses ordinary byte vectors and applies offset writes and resizing
independently of the driver's storage representation. The workload crosses
resident/non-resident boundaries and includes holes, overwrites and shrinking.

After every operation the writer is synced and closed. Fresh driver and
upstream `ntfs` readers compare directory names, file sizes and complete bytes
with the model; `check_io` must report no findings. Each sequence has a
90-second deadline. A failure retains its seed, geometry and operation trace
under `tmp/op-sequence-model/`; rerunning its named test reproduces it.

`chore test:suite` includes these four tests. `chore test:op-sequence-oracle`
additionally requires ntfs-3g, grades each final image with `ntfsfix -n` and
compares every final file with `ntfscat`. Missing tools fail with installation
instructions. CI runs this hook on Ubuntu and retains traces, refusal counts
and reference-tool reports as an artifact. It uses the existing mkfs budget;
no budget or executed-case floor is lowered.

This slice does not claim Windows chkdsk validation. The Windows matrix,
broader operation generation (rename, links, directories, ADS and EAs), larger
explorer seed ranges, script replay and minimization remain under #484.
