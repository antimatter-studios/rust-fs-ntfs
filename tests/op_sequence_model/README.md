# Seeded operation readback: first slice of #484

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
