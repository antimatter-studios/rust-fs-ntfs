# Operation-sequence test support (#484)

`generator.rs` adds #681's nine operations: create, mkdir, full write,
write_at, truncate, same-directory rename, unlink, rmdir and link, then
#731's write_stream and delete_stream, and #732's write_ea and remove_ea.
It reuses the preserved engine's operation shapes and collision/UTF-16
name cases.
`Generator::next(&model)` chooses from the live tree after each disk result;
`decode(bytes, &initial_model)` instead advances a cloned model, retaining
refused operations. Both need the same initial tree and injected name table.
No driver API, host filesystem, RNG, clock or locale supplies decisions.

Input uses eight-byte headers: opcode modulo 13 in the order above, target,
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
not POSIX dot entries. Timestamps and open handles are outside this
slice; named streams (#731) and EAs (#732) follow below. `harness.rs` wires it into disk execution; #683–#685 cover the explorer,
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
`tmp/op-sequence-model/`. Windows chkdsk
remains under #685; this test-only slice changes no format code.

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

## Named data streams (#731)

The model keeps a node's named `$DATA` streams beside its unnamed data,
keyed through the volume's `$UpCase` like file names: `S` written to a file
holding `s` replaces `s`'s bytes and keeps the spelling `s`. Streams belong
to the node, so every hard link shows the same ones, and the last unlink or
replacing rename discards them. A stream name is refused when it is empty,
`.` or `..`, longer than 255 UTF-16 units, holds a reserved or control
character, or ends in a space or period, which Win32 path parsing strips.
Deleting a stream that is not there is refused. Stream bytes never count in
a file's size.

Opcode 9 is write_stream, which consumes its payload as a write does, and
10 is delete_stream. Their target selector chooses a missing path (0), a
file used as a directory (1) or an existing file (2/3); their name selector
indexes a stream-name pool of `$UpCase` collisions, reserved characters, a
trailing period and space, a control character, the 255/256-unit
boundaries, an existing stream of the target (19) and a fresh name (20).
The model accepts streams on directories, but the generator does not aim at
them yet: `rmdir` gives back a directory's stream clusters only when the
directory has index blocks (#735).

Every step compares each modeled path's streams, names and complete bytes,
on the synced writer and through fresh read-only descriptors, and again
through the upstream `ntfs` parser, which enumerates `$DATA` attributes
itself and follows `$ATTRIBUTE_LIST`s. The ntfs-3g hook reads every stream
with `ntfscat -n` and compares it with the model. Shrinking shortens a
stream write's payload as it does a write's.

Three refusals depend on where the driver put a file's attributes, which
the model does not know, so the harness accepts them when the model
accepted the operation: W2.1's mapping capacity for unnamed data, the
`$ATTRIBUTE_LIST` that a stream too big for its base record gives a file
(#480), which the name-changing paths then refuse (#468), and the
attributes `attr_list` documents it does not move. Each still has to leave
the whole snapshot unchanged, and each is recorded in `refusals.tsv` with
its reason. `fuzz/corpus/operation_sequence/streams-v1.json` seeds the
explorer with the stream cases; histograms now have eleven slots.

## Extended attributes (#732)

The model keeps a node's EAs as `FILE_FULL_EA_INFORMATION` describes them:
a byte-string name, a flags byte and a value. A name is stored upper-cased,
as Windows' `NtSetEaFile` stores it, and a write whose name matches an
existing one case-insensitively replaces that entry, flags and all. Only
ASCII letters fold. EAs belong to the node, so every hard link shows the
same ones, and the last unlink or replacing rename discards them. A name is
refused when it is empty, longer than 254 bytes or holds a NUL, which ends
a name on disk. Removing a name that is not there is refused. A write is
refused, with nothing changed, when the file's set would pack to more than
the 65,535 bytes `$EA_INFORMATION`'s `EaPackedLength` counts: five bytes per
entry beside its name and value. An empty value is an EA, as the library's
`write_ea` stores one; EAs never count in a file's size.

Opcode 11 is write_ea and 12 is remove_ea. Their target selector chooses as
the stream opcodes' does: a missing path (0), a file used as a directory (1)
or an existing file (2/3). Their name selector indexes a pool of ASCII case
collisions, the empty name, the 254/255-byte boundaries, an existing EA of
the target in lower case (12) and a fresh name (13). The parent byte's low
bit sets `NEED_EA`. A write's value reaches past 64 KiB from a bounded
payload: the offset's low byte picks a size class for the length (under
128, 2,048 or 16,384 bytes, or the whole u16), at most 4,096 payload bytes
are consumed and repeated to that size, and with none left a fixed pattern
fills it. The model accepts EAs on directories, but the generator does not
aim at them: the driver keeps a directory's EAs in its base record, so how
large a set it takes depends on where that record stands.

Every step compares each modeled path's EAs, by stored name with flags and
complete value, on the synced writer through `list_eas_io` and through
fresh read-only descriptors. The upstream `ntfs` parser then finds the raw
`$EA` and `$EA_INFORMATION` values itself, following `$ATTRIBUTE_LIST`s and
cluster runs, and the harness decodes them from MS-FSCC 2.4.15 rather than
with `ea_io`: NUL-terminated names, four-byte-aligned `NextEntryOffset`s, a
zero offset on the last entry and no bytes after it but its padding.
`$EA_INFORMATION` has to agree with the decoded set: its packed length,
`NEED_EA` count and the length of the set as aligned query entries. A file
with no EAs has neither attribute. The ntfs-3g hook reads every file's `$EA`
with `ntfscat -a 0xE0` and decodes it the same way. Shrinking shortens an EA
write's value. The refusals `attr_list` documents for streams are accepted
for EA operations on the same terms.
`fuzz/corpus/operation_sequence/eas-v1.json` seeds the explorer with the EA
cases; histograms now have thirteen slots.
