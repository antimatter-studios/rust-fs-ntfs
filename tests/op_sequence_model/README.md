# Operation-sequence test support (#484)

`generator.rs` adds #681's nine operations: create, mkdir, full write,
write_at, truncate, same-directory rename, unlink, rmdir and link. It reuses
the preserved engine's operation shapes and collision/UTF-16 name cases.
`Generator::next(&model)` chooses from the live tree after each disk result;
`decode(bytes, &initial_model)` instead advances a cloned model, retaining
refused operations. Both need the same initial tree and injected name table.
No driver API, host filesystem, RNG, clock or locale supplies decisions.

Input uses eight-byte headers: opcode modulo 9 in the order above, target,
parent, name, little-endian u16 length/size, little-endian u16 offset. Partial
headers are zero-padded; writes consume at most `length % 4097` remaining
payload bytes. Selectors modulo 4 choose missing (0), wrong type (1) or
existing (2/3) paths; name selectors 19/20 choose an existing/fresh name.
The opcode's high bit enables replacement rename. Truncate size 65535 means
65536. Bounds are 120 operations, 4096 data bytes per write, 64 KiB file
extents, four directory levels, 4096 path bytes and 256 UTF-16 name units
(including deliberate invalid names). Extra input after 120 steps is ignored.

`encode` / `decode_replay` store concrete operations in strict version-1 JSON,
capped at 4 MiB. The named `reproducers/basic-write-v1.json` golden is consumed
by `tests/op_sequence_generator.rs`; the libFuzzer operation target calls
the same decoder. Replay does not reselect paths when steps are removed.
Eight fixture-free CI contracts run on every host leg with an executed floor.

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
slice. `harness.rs` wires it into disk execution; #683–#685 cover the explorer,
regression capture and extensions.
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

`harness.rs` executes every operation from the live byte generator or concrete
JSON replay through the library. It commits a cloned model only after a full
successful disk result. A refused operation must also refuse in the model,
except the original gate's documented W2.1 mapping-capacity refusal. Full
snapshots, including parent times/flags, references, names and bytes, must
remain unchanged after every refusal. Expected bytes never come from a reader.
Every step compares the whole tree on the synced writer and again through
fresh read-only descriptors: names, kinds, sizes, link counts, alias identities
and complete logical bytes. An independent `ntfs` parser checks names and
metadata; it cannot check zero-filled uninitialized tails, which `ntfscat`
checks in full at the end. Every eighth step and the end validate a fresh
whole-image mount and run `check_io`. Final ntfs-3g checks include every modeled
directory with `ntfsls -a -p` and every file with `ntfscat`.

Four additional contracts cover all nine generated operation kinds, refusals,
nested replay and deliberate persisted byte/name mutations. All eight readback
tests run on each native CI host with an explicit floor, and the Ubuntu hook
requires all three reference tools. Scratch images use the shared panic-safe
owner. Concrete prefixes, input bytes, geometry and refusals are retained under
`tmp/op-sequence-model/`. Windows chkdsk and ADS/EAs
remain under #484; this test-only slice changes no format code.

The committed `fuzz/corpus/operation_sequence/*.json` corpus uses the generator's
strict JSON v1 format. `committed_corpus_replays_every_seed` enumerates every
JSON seed, requires all three named baseline seeds, and writes its executed
manifest to `tmp/op-sequence-model/corpus-executed.json`. The ordinary
`chore test:suite`, native readback CI step and ntfs-3g hook all replay it.
Existing test floors and measured output budgets remain unchanged; raise a
floor only after a CI measurement. Missing seeds or tools fail.

The existing `fuzz.yml` workflow also runs libFuzzer's `operation_sequence`
target, preserving all byte-decoder targets. `seconds` (1..3600, default 120)
sets exploration time; `seed` (1..4294967295, default 683) is logged before
execution. Each input has a 90-second limit, 768 MiB allocation limit and
1024 MiB RSS limit. A process-group watchdog allows one final input, then
terminates at `seconds + 90` and kills at `seconds + 95`; any timeout fails.
Compilation is outside that clock. The explorer inherits the measured suite
output budget, keeps the complete raw log and requires executed input stats.

Artifacts retain the run seed/budgets, raw input, concrete JSON prefix,
geometry, per-opcode counts, cumulative accepted/refused histogram, ntfs-3g
reports and a failure image when available, on success and failure alike.
Histogram arrays follow opcode order above, accepted first, refused second.
Replay an uploaded raw input or `reproducer.json` from the repository root:

```sh
OPSEQ_REFERENCE_TOOLS=1 cargo +nightly fuzz run operation_sequence /absolute/path/to/input.bin -- -runs=1 -timeout=90 -rss_limit_mb=1024
```

For a fresh bounded run on Linux with cargo-fuzz, coreutils and ntfs-3g:
`OPSEQ_FUZZ_SEED=683 bash scripts/fuzz-op-sequence.sh 120`.

## A failing sequence becomes a permanent regression (#684)

One command, run from the repository root on a retained `reproducer.json`
or a raw libFuzzer input, minimizes the failure and pins it in the corpus:

```sh
cargo run --release --example op_sequence_capture -- tmp/op-sequence-model/explorer/reproducer.json truncate-keeps-old-size
```

A mismatch is its kind and its step. The kind is the failure message's first
line, after any `assertion ... failed: ` prefix, up to the first `: `, path
(` /`), `{` or `"`, so paths, values and operands do not count. The step is
the number of operations executed when it was seen. `shrink.rs` keeps the
operations through that step, then deletes runs of earlier operations in
halving chunks and shortens write payloads (keeping their prefix), write
offsets and truncate sizes, smallest candidate first, until neither changes
anything. A candidate counts only when the same kind follows its own last
operation. Paths and names are never rewritten, because later operations name
them. Every candidate is a whole replay on a fresh image; nothing depends on
the generator's bytes, a seed or a PRNG.

The result is `fuzz/corpus/operation_sequence/<name>-v1.json`: the strict v1
operations plus a `regression` object pinning the kind, the step, the image
geometry (`format_filesystem`'s size, cluster, record, label and serial) and
the original input's path and operation count. Replay ignores the pin.
`committed_corpus_replays_every_seed` replays the entry in every suite run,
prints the seed and its pin if it fails, retains its evidence under
`tmp/op-sequence-model/corpus-<name>-v1/`, and refuses a pin recorded on other
geometry. Capture never overwrites an entry and only reads its input. An
input that replays cleanly or no longer reproduces from its own prefix, or
that lies in the capture's scratch directory
(`tmp/op-sequence-model/capture-<name>/`), fails with the input named and
unchanged, and pins nothing.

For a real finding, commit the entry alone first: the suite fails on it with
the pinned kind. Then fix it on the same branch, leaving the entry untouched.
Our model and our readers share one interpretation of NTFS, so before fixing,
attach evidence from readers that are not ours to the issue. The failing run
leaves `failure.img` beside the entry's evidence: grade it with ntfs-3g's
`ntfsfix -n`, list and read the paths the pinned operation touched with
`ntfsls -a -p` and `ntfscat`, and keep those reports with the image's SHA-256.
When the finding is about bytes on the volume, also grade the image with
Windows chkdsk (`windows-test-skill`) or Linux's ntfs3 (`chore test:kernel`).
If the independent readers agree with the driver, the model is wrong: fix the
model, never the pinned entry. Once fixed, `chore test:op-sequence-oracle`
grades the entry's final image with ntfs-3g on every CI run.

`op_sequence_readback.rs` proves the loop with a test-only `StaleTruncate`
wrapper that keeps reporting a file's old size after a shrinking truncate. An
eighteen-operation workload of every kind shrinks to a create, a one-byte write
and a truncate to zero, which the real model and the disk agree on. Capture
contracts cover raw generator bytes, the written pin and refused overwrites,
clean inputs, scratch-directory inputs and names. The pin format has its own
fixture-free contract in `op_sequence_generator.rs`.
