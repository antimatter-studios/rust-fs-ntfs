# rust-fs-ntfs — pure-Rust NTFS driver

A pure-Rust read/write NTFS driver, dual-licensed Apache-2.0 / MIT,
with no kernel dependencies and no FFI to a C-language NTFS library.
The crate ships a stable C ABI (`fs_ntfs_*`) so it can be linked
from C, C++, Go (via cgo), Swift, or any other language with FFI.
Mount + write of freshly-formatted volumes is validated end-to-end
against Microsoft's `chkdsk` running on real Windows VMs — that's
the test contract, not byte-equivalence with any third-party tool.

## Status

In active development, not yet 1.0. Reading covers resident, non-resident,
fragmented, sparse and LZNT1-compressed data, attribute lists, alternate data
streams, reparse points, extended attributes and Unicode names; WOF-compressed
and encrypted files are refused. Writing covers data, create, unlink, mkdir,
rmdir, rename, hard links, streams, reparse points, extended attributes,
timestamps and flags, in directories of up to 64 index blocks. `$LogFile` is
replayed as Windows' own restart replays it, on `fsck` and on a read-write
mount that asks for it; any other read-write mount refuses a log holding
work, which Windows also leaves after a Fast Startup shutdown, and a log
that cannot be replayed in full is refused with nothing written. `mkfs.ntfs` makes volumes Windows mounts and writes and `chkdsk`
accepts. **[docs/features.md](docs/features.md) is the full list**: every
feature, its state (supported, partial, refused, not supported or upcoming),
the release it shipped in, its tracking issue or write-plan item, and the test
that checks it. Every pull request that changes behaviour updates it.

## Scenario field translations (NTFS ↔ harness)

The test matrix uses harness-level generic field names rather than
NTFS-native terminology, so the same scenario shape composes with
sibling fs-* drivers. If you came here looking for an NTFS-native
name and can't find it in `test-matrix.json`, this table is where
to look:

| NTFS-native name | Harness name      | Notes |
|------------------|-------------------|-------|
| `cluster_size`   | `alloc_unit_size` | Same concept, generic name. Valid: 512, 1024, 2048, 4096, ..., 65536. The legacy `cluster_size` key is still accepted as a serde alias during the v1->v2 transition. |
| chkdsk verdict   | `verdict_shape`   | Mapped to `clean` / `repair-ok` / `repair-required`. |
| operation_sequence string (v1) | `recipe[]` array (v2) | The v1 arrow-string `mac:format -> win:chkdsk(readonly,/scan)` becomes a v2 recipe of typed steps with `host: "host" / "vm"` per step. |

Cross-driver vocabulary index lives in
[`docs/vocabulary.md` in fs-windows-test-harness](https://github.com/antimatter-studios/fs-windows-test-harness/blob/main/docs/vocabulary.md);
contributor-facing translation rules + bloat-prevention conventions
are documented there.

## Test contract

- **Lib unit tests:** 500+ (`cargo test --lib`).
- **Integration tests:** 600+ across ~90 binaries. Some are marked
  ignored (long-running `cluster_size_matrix`, and the matrix
  harness on non-Windows hosts).
- **Coverage areas (one or more dedicated test files each):**
  reads, every C-ABI entry point, round-trip writes, corruption
  fuzz under concurrent read-back, Unicode names, large files,
  sparse reads, deep directories, rename / resize variants, fsck
  on both path and callback transports, ADS combinatorics, EA
  combinatorics, end-to-end mac→Windows workflows.
- **Static images** for fixtures awkward to build programmatically
  live under `test-disks/`. Most tests assemble their NTFS image at
  runtime.
- **chkdsk validation:** the `test-matrix.json` matrix runs through
  the `fs-windows-test-harness` runner (`scripts/run-matrix.sh`, see
  [Test matrix](#test-matrix-windows--macos-vm-coordination)), which on Windows
  shells out to `rust-ntfs format`, Microsoft's `format.com`, and
  Microsoft's `chkdsk` to validate every formatted image. On non-
  Windows hosts the matrix tests are reported as ignored. Microsoft's
  `chkdsk` is the authoritative validator — not byte-equivalence with
  any third-party formatter.
- **Test matrix:** `test-matrix.json` at repo root carries the
  scenarios. The harness drives mac-side ops (format, populate via
  the pure-Rust write API) and Windows-side ops (mount, chkdsk,
  enumerate, write, repeat-mount stability cycles) through a single
  declarative JSON contract; results from a Windows VM stream back
  over SSH. See `fs-windows-test-harness.toml` for the op declarations and
  `../fs-windows-test-harness/` for the runner.
- **Fuzz:** `fuzz/` carries cargo-fuzz harnesses for the three
  byte-decoders most likely to regress (data-runs, attribute headers,
  INDX block headers).
- **Bench:** `benches/byte_decoders.rs` (Criterion). Not part of CI.

## Changelog

Reverse chronological highlights from `git log`. Full per-commit
history available via `git log` in the repo.

### 2026-10-14 — 0.10.2

- Reparse points, symlinks and object ids are entered in
  `$Extend\$Reparse:$R` and `$Extend\$ObjId:$O`, and taken out with the
  attribute or the file, from the root or an INDX block; a second file is
  refused an object id another has.

### 2026-10-13 — 0.10.1

- Unlinking a file's last name frees its extension records and every
  cluster they map, where ntfs-3g or Windows spread its attributes over
  them. They used to stay in use with no base record.
- The first published release of everything 0.10.0 lists: 0.10.0 was
  tagged, and its release run stopped before `cargo publish`.

### 2026-10-13 — 0.10.0

- **Breaking:** fsck leaves the dirty flag set for Windows' chkdsk unless
  asked to clear it (`--clear-dirty`, `FsckOptions::clear_dirty`,
  `FS_NTFS_FSCK_CLEAR_DIRTY`); the C `fs_ntfs_fsck*` functions take a
  `flags` argument. A read-write mount replays a log holding work only when
  asked, and a volume Windows hibernated is refused for writing.
- `check_io` accounts for every cluster against `$Bitmap` and checks each
  record's attribute chain. `$MFTMirr` holds a whole cluster of records on
  large clusters, as Windows' formatter writes it.
- `fs.ntfs rm` and `rmdir`. Writes advance the times of what they change;
  names, streams, EAs, symlinks and renames are written the way Windows
  writes them; a record with an `$ATTRIBUTE_LIST` is refused any change
  that would need the list maintained.
- Linux's ntfs3 and ntfs-3g read what this crate writes, and a volume
  ntfs-3g populated reads back byte for byte.

### 2026-10-06 — 0.9.0

- The crate is published as `rust-fs-ntfs`, the repository's name, and the
  repository moved to the antimatter-studios organisation. A dependent
  changes one line, `am-fs-ntfs = "0.8"` to `rust-fs-ntfs = "0.9"`; the
  import (`fs_ntfs`) and the C symbols are unchanged. It depends on
  `rust-fs-core` 0.3.0.

### 2026-10-06 — 0.8.0

- The last version published as `am-fs-ntfs`, with a description and README
  naming `rust-fs-ntfs` as its successor. See CHANGELOG.md for what else it
  carries.

### 2026-10-02 — 0.7.0

- **Breaking:** `fsck` decides from `$LogFile` on every volume and never
  writes it. A log that may hold transactions is refused, dirty flag or
  not, instead of being overwritten; the C `fs_ntfs_fsck*` functions lose
  their `out_logfile_bytes` parameter, and `FsckReport.logfile` says
  whether the log was empty or clean.
- **Breaking:** the Rust API's errors are `fs_ntfs::error::Error`, with a
  `Kind`, instead of `String`. The message is unchanged, and the errno is
  decided where the error is raised rather than guessed from its words.
- A read-write mount refuses a volume whose `$LogFile` may hold
  transactions, which Windows 8 and later leave behind an unclean
  shutdown with the dirty flag clear. A read-only mount is still allowed.
- Pull requests are refused when they break the public API without the
  version saying so.

### 2026-09-30 — 0.6.0

- The command-line tools are one multi-call binary, `rust-fs-ntfs`,
  behind a new `cli` cargo feature: `mkfs.ntfs`, `fsck.ntfs` and
  `fs.ntfs` are names it answers to, and `rust-fs-ntfs doctor` says
  whether each name on PATH is ours.
- `fs.ntfs` lists, reads and reports a volume without mounting it, and
  writes files from stdin, makes directories and sets the label and the
  dirty flag; Windows reads every write back and finds the volume clean.
- `fsck.ntfs` checks the dirty flag, `$LogFile`, `$MFTMirr` and every
  in-use MFT record, with fsck(8)'s exit statuses, and clears the dirty
  flag only over an empty log.
- The tools write their own man pages and shell completions, and a
  release attaches them as attested install-prefix tarballs for macOS
  (arm64) and Linux (x86_64).
- The `mkfs_ntfs` cargo target is gone; the formatter is the `mkfs`
  tool of `rust-fs-ntfs`.

### 2026-09-27 — 0.5.0

- **Breaking:** `fs_ntfs_readlink` follows the readlink contract shared
  by every driver in the family. Success returns the target's length,
  not counting the NUL; a short buffer is `ERANGE` with nothing written;
  a path that is not a link, or a NULL argument, is `EINVAL`. Callers
  that test `== 0` for success must switch to `>= 0`.
- **Breaking (Rust API):** `idx_block::vcn_to_disk_offset` takes a third
  argument, and `index_io::IndexEntryLocation` gains a required field.
- The read and write paths are bounded against the volume: offsets,
  run ends, index blocks and allocations are checked before they are
  used, and a failure after an allocation gives the clusters back.
- `$ATTRIBUTE_LIST` is consulted before the base record, index lookups
  collate the way the index is ordered, and a WOF-compressed file is
  refused by the Rust API as well as the C ABI, rather than read back
  as zeros.
- mkfs system streams carry valid headers and the measured namespace,
  `$MFTMirr` declares the four records it holds, and a volume-label
  change refreshes it.
- The fuzz harness runs and covers more decoders; the test tiers print
  a verdict rather than a transcript.

### 2026-09-06 — 0.4.0

- `mkfs.ntfs` ships as its own binary, sharing the formatter with the
  subcommand rather than duplicating it.
- An index edit stays inside the index it is editing. For an
  `$INDEX_ROOT` the buffer is the whole MFT record and the index lives
  in one attribute's resident value; the shift and the zero-fill were
  bounded by the record, so they ranged over every other attribute and
  the shredded record was written back reporting success.
- Removing or inserting an index entry consults `IE_FLAG_HAS_SUBNODE`.
  An entry may carry a child VCN, and shifting it away orphaned the
  whole subtree — invisible to this crate's own reader, which scans
  rather than descends, and visible to chkdsk.
- A file's runs may not free the volume's own clusters. `unlink` and
  `truncate` pushed every run into the bitmap bounded only by
  `$Bitmap`'s own size, so a record whose runs overlap `$MFT` freed
  live system clusters.
- A write offset is checked against the volume before anything is
  written.
- The driver is bounded against the bytes it is handed at the trust
  boundary: MFT record validation, on-disk lengths used as allocation
  sizes, an unclamped `$LogFile` fill, index-entry name walks, and two
  bitmap walks that could fail to terminate.

### 2026-09-04 — 0.3.5

- Dropped the git submodules: `rust-fs-core` and the Windows test harness
  now resolve from sibling checkouts pinned by `chore siblings`, matching
  the rest of the family. This was the last repo still using submodules.
- The lockfile moves `rust-fs-core` 0.2.2 → 0.2.4 — it had been two
  releases behind, which is the drift a per-consumer submodule pin hides.
- Covered the create rollback: disabling either half of
  `undo_new_record_io` had left the suite green. Four tests now pin it.
- `clear_in_use: bool` → `NewRecordState::{BitmapOnly, RecordWritten}`.

## License

Dual-licensed Apache-2.0 ([`LICENSE-APACHE`](LICENSE-APACHE)) or
MIT ([`LICENSE-MIT`](LICENSE-MIT)) at your option, matching the
upstream `ntfs` crate. The standard Rust-ecosystem dual-license
pattern.

`Cargo.toml` declares `license = "MIT OR Apache-2.0"`.
`deny.toml` enforces a permissive-only allowlist (MIT, Apache-2.0,
BSD-2-Clause, BSD-3-Clause, ISC, Unicode, CC0-1.0, Zlib, 0BSD)
across transitive dependencies. CI checks that allowlist with
`cargo deny --all-features check`.

## Building

Standard cargo:

```sh
cargo build --release
# → target/release/libfs_ntfs.{a,rlib}
cargo build --release --features harness --bin rust-ntfs
# → target/release/rust-ntfs   (the test matrix's driver: format / ls / touch / mkdir / write / rm / rmdir / set_dirty)
```

`rust-ntfs` is behind the test-only `harness` feature, so `cargo install
rust-fs-ntfs --features cli` puts one program on PATH: `rust-fs-ntfs`, the
command-line tools below.

### Command-line tools

The user-facing tools are one multi-call binary, `rust-fs-ntfs`, behind
the `cli` feature so the static library gains no dependency from them. It
answers to the name it is started under, so an install links each dotted
name to it:

```sh
cargo build --release --features cli --bin rust-fs-ntfs
chore cli:install                  # stage tmp/cli/bin (every name) and tmp/cli/share (man pages, completions)
export PATH="$PWD/tmp/cli/bin:$PATH"
mkfs.ntfs --size 64M --label BACKUP disk.img   # a JSON report on stdout
rust-fs-ntfs mkfs --help           # the same tool; nothing can shadow this name
fs.ntfs disk.img ls /              # JSON entries; --all adds the metafiles
fs.ntfs disk.img ls -R --max-depth 8 --limit 1000 /Users
fs.ntfs disk.img stat /notes.txt   # one path: record, links, sizes, every time
fs.ntfs disk.img streams /notes.txt   # named data streams; JSON or --text
fs.ntfs disk.img xattr /notes.txt     # native NTFS EAs; JSON or --text
fs.ntfs disk.img xattr /notes.txt user.comment -o comment.bin
fs.ntfs disk.img read /notes.txt > notes.txt
fs.ntfs disk.img read --stream Zone.Identifier /notes.txt
fs.ntfs disk.img get label --text
fs.ntfs disk.img write /notes.txt < notes.txt
fs.ntfs disk.img write --at 4095 /notes.txt < patch.bin   # patch without truncating
fs.ntfs disk.img truncate /notes.txt 4097   # length in bytes; a grown tail reads as zeros
fs.ntfs disk.img mkdir /backup
fs.ntfs disk.img mkdir -p /backup/2026/october   # every missing directory
fs.ntfs disk.img rm /notes.txt
fs.ntfs disk.img rmdir /backup     # an empty directory
fs.ntfs disk.img mv /notes.txt /notes-2026.txt   # a new name, same directory
fs.ntfs disk.img ln /notes.txt /backup/notes.txt   # another name, a hard link
fs.ntfs disk.img ln -s ../notes.txt /backup/latest.txt   # a symbolic link
fs.ntfs disk.img set label "Backup Volume"
fs.ntfs --offset 1048576 whole-disk.img info
fsck.ntfs disk.img                 # dirty flag, $LogFile, $MFTMirr, MFT records; exit 0/1/4/8
fsck.ntfs --dry-run disk.img       # every write -y would make, by unit; none is made
rust-fs-ntfs doctor                # is every name on PATH this program?
chore test:cli                     # the tools as installed: doctor, then tests/cli/
chore test:cli:oracle              # against Windows-written volumes, and ntfs-3g (tests/cli-oracle/)
chore test:cli:host                # the Windows matrix's cli steps that run on this machine, no VM
```

`streams PATH` lists named `$DATA` streams, excluding the unnamed stream,
as rows with `name`, `size`, `allocated`, `resident`, `sparse` and `compressed`.
Sizes are bytes; allocation includes resident value space inside the record
or the clusters actually allocated to non-resident data. Names are sorted
as UTF-8 bytes, including streams held in extension records. Directories
can have named streams. Links are inspected without following them, and a
colon in PATH is never reinterpreted as a stream selector. The command opens
the source read-only. Plain `read` continues to return the unnamed stream.
`read --stream NAME PATH` reads one named stream with Windows case-insensitive
name lookup. It uses the same bounded chunks and `-o FILE` atomic copy as
plain `read`; a missing stream exits 1 naming NAME, without stdout or changes
to an existing output file. Directories and links keep `read`'s refusal rules.
Selection requires `--stream`: a colon in PATH never selects a stream.
The installed CLI stream tests need ntfs-3g's `ntfscp` to author independent
fixtures (`apt-get install ntfs-3g`); the Windows oracle tier compares each
stream with bytes captured by `Get-Content -Stream NAME -AsByteStream`.

`xattr PATH` lists native NTFS `$EA` entries with their stored `name`, value
`size` in bytes and `ntfs.need_ea` boolean. Names retain their namespace prefix;
a non-UTF-8 name is shown lossily and adds `name_hex` with its original bytes.
These are NTFS EAs, separate from POSIX xattrs and named data streams.
`xattr PATH NAME` writes that EA's raw value to stdout, or atomically to
`-o FILE`, even with `--text`. Selection ignores ASCII case, as the native EA
library does. A missing path or name exits 1 with no stdout and preserves an
existing output file. Directories and links are inspected without following
links. All inspection opens the image read-only and honors `--offset`.
The Windows fixture producer captures `NtQueryEaFile` names, sizes, flags and
value bytes for the CLI oracle tier, including binary values and large EA sets.

`ls --recursive` (`-R`) walks depth first with each directory's names sorted
as bytes. Entries add `path` from the volume root and `depth`; the starting
directory's entries are depth 1. A file or reparse starting point is listed
alone at depth 0. Symlinks, junctions and all other reparse points are listed
without being followed; hard links appear under every name. Directory records
already visited add `"revisited": true` and are not entered again. Metafiles
stay hidden unless `--all` asks. Plain `ls` still lists only one directory.
`--max-depth` defaults to 64 and `--limit` to 100000; both require `-R`.
An exact fit succeeds. One more entry past either bound, or an unreadable
directory, exits 1 with the complete partial report on stdout and the bound
or stopping path named on stderr. A missing starting path prints no report.

`mv SOURCE PATH` gives SOURCE the name PATH, which is the new name itself,
not a directory to move into, and must be in SOURCE's directory. A move to
another directory answers `not implemented` (exit 3) and changes nothing.
A name already at PATH is refused rather than replaced, with names compared
as Windows compares them, without regard to case; a case-only rename of
SOURCE itself is allowed. Everything that can be refused -- a missing
SOURCE, a name Windows cannot address, the root, `.`, `..`, a metafile, a
dirty volume -- is refused before the image is opened for writing, so it
stays byte-identical. The report is `{"path": ..., "moved": true}`.

`ln TARGET PATH` gives the file at TARGET the further name PATH, in any
directory; `ln -s TARGET PATH` makes PATH a symbolic link whose target is
TARGET, the text as given: relative to PATH's directory, from the volume's
root when it starts with `/`, or a drive path. The link is a file link, as
`mklink` makes one without `/D`. A directory is given no second name, and a
file no more than the 1024 Windows allows. A name already at PATH, compared
as Windows compares names, a missing directory, a name Windows cannot
address, a metafile and a dirty volume are all refused before the image is
opened for writing, so it stays byte-identical. The report is `{"path":
..., "linked": true, "inode": N}`, N being the file's record for a hard
link and the new link's for a symbolic one.

`write --at BYTES PATH` writes all of stdin at a byte position in an existing
regular file, preserving the prefix and suffix. A write past EOF grows the
file and zero-fills the gap, as Windows does; an empty input changes nothing.
Missing paths, directories and read-only files fail with exit 1. Symbolic
links and other reparse points are never followed (exit 3); compressed,
sparse and encrypted streams this driver cannot patch also answer exit 3.
Input is buffered before opening the image, and the full edit is rehearsed
before a writable open, so predictable refusals leave it byte-identical.
The report is `{"path": ..., "at": N, "bytes": N, "size": N}`. Plain `write`
still creates or replaces; `--offset` stays the filesystem's position inside
TARGET, independent of `--at`.

`truncate PATH SIZE` sets a regular file's length to SIZE bytes, an unsigned
decimal count without a suffix. Shrinking frees whole clusters past the new
end; growing preserves the prefix and reads back zeros in the new tail.
Resident data can grow into non-resident data. Directories and missing paths
fail with exit 1; symbolic links, other reparse points, and compressed, sparse
or encrypted data the driver cannot resize answer exit 3. Edits are rehearsed
before a writable open, so any predictable refusal leaves the target
byte-identical. The report is `{"path": ..., "size": N}`; `--offset` remains
the filesystem's position inside TARGET.

`mkdir --parents` (`-p`) makes every missing directory on the way to PATH;
a directory already at PATH, or at any step, is fine. Names are found as
Windows finds them, without regard to case, and `.` and `..` are taken from
the text of PATH as Windows takes them, so `/a/../b` makes only `/b`. A file
or a symbolic link on the way or at PATH (no link is followed), a name
Windows cannot address and a dirty volume are refused before the image is
opened for writing, so a refused `mkdir -p` makes no part of the chain; the
whole chain is then made in one writable open. The report is `{"path": ...,
"created": [...]}`, listing only the directories made, as PATH spells them.
Plain `mkdir` still needs PATH's parent to exist.

Reports are JSON on stdout (`--text` for people); a failure is
`{"error": "...", "code": N}` on stderr, `N` being the exit status. A
release attaches `rust-fs-ntfs-<version>-<platform>.tar.gz` for macOS arm64
and Linux x86_64, laid out as an install prefix (`bin/`,
`share/man`, the zsh, bash and fish completions, `share/rust-fs-ntfs/CAVEATS`,
the licences).

Universal macOS static lib (aarch64 + x86_64 lipo'd):

```sh
./build.sh           # → dist/libfs_ntfs.a
```

The Rust toolchain is pinned in `rust-toolchain.toml`; cargo picks
it up automatically.

### Tests

```sh
chore test                     # every tier CI runs, quietly, then one total
chore test:unit                # one tier: unit | unit:debug | mkfs | suite
chore test -- --verbose        # the same run, streamed as it happens
cargo test --test capi_fsck_callbacks   # one file, straight through cargo
```

The full suite requires the Windows-authored `test-disks/ntfs-attrlist.img`,
`test-disks/ntfs-compressed.img`, `test-disks/ntfs-symlink.img`, the
symlink targets Windows reports, `test-disks/ntfs-symlink.targets.tsv`, and
`test-disks/ntfs-format-64k.img`, an empty volume Windows formatted with
64 KiB clusters.
Generate them with
`test-disks/build-windows-native-read-fixtures.ps1` on Windows; CI uploads
the same images to its Linux integration job.

**Quiet by default.** Each tier prints one verdict line naming its log,
and the whole run goes to `tmp/logs/<tier>.log`:

```text
unit: ok (653 lines, 44377 bytes) — …/tmp/logs/unit.log
```

A failure prints the last 40 lines of the log instead. A tier that passed
but printed more than its budget fails with exit status 65, apart from a
failing suite: the budgets are measured, they live in one table in
`scripts/tier-budgets.txt`, and CI runs every `cargo test` through the same
runner. The runner and the wrapper are rust-fs-core's `scripts/tier.sh` and
`scripts/output-budget.sh`, run in place from the sibling `../rust-fs-core`
checkout at the pinned version; this repository keeps no copy. The design and its token-efficiency tradeoffs are documented in
[`docs/output-budget-token-efficiency.md`](docs/output-budget-token-efficiency.md).
The broader coordination and workflow benefits are summarised in
[`docs/agent-workflow-benefits.md`](docs/agent-workflow-benefits.md).
`test:suite`
needs the `test-disks/ntfs-*.img` fixtures (built on Linux by
`test-disks/build-ntfs-feature-images.sh`) and says so if they are
missing.

### Test matrix (Windows + macOS VM coordination)

The chkdsk-validated matrix lives in `test-matrix.json` at the repo
root and runs through
[`fs-windows-test-harness`](https://github.com/antimatter-studios/fs-windows-test-harness),
a sibling checkout at `../fs-windows-test-harness/` pinned in
`chores.yml` (`chore siblings` fetches it). The ops this crate declares
are in `fs-windows-test-harness.toml`; the Windows-side PowerShell they
run is in `scripts/fs-windows-test-harness/`.

**The Windows VM is the harness's persistent Windows ARM64 guest under
QEMU**, and setting it up is the harness's documentation, not this crate's:
follow
[`docs/qemu-vm.md`](https://github.com/antimatter-studios/fs-windows-test-harness/blob/main/docs/qemu-vm.md)
in the harness. It runs on Linux ARM64 with KVM (a Raspberry Pi 5 among
them) and on Apple Silicon macOS with HVF: install the guest once, and every
run after that reuses it. Then `chore vm:setup` provisions what this crate
needs on it (`format.com` / `chkdsk` tooling and `rust-img-vhd`).

This machine's VM settings go in a gitignored `.test-env` at the repo
root. The harness reads it as shell, so **quote any value with a space**:

```sh
VM_HOST=fswth-local                    # the guest, as `scripts/vm.sh exec` names it
SSH_KEY=                               # empty: the guest's own ssh config names its key
VM_WORKDIR=C:/fswth/rust-fs-ntfs       # this crate's work directory on the guest
HOST_IMAGE_DIR=/tmp
VM_STATE=/path/to/guest/state          # only if not the harness's default state directory
```

The VM is started and stopped by chore, which runs the harness's
`scripts/local-vm.py` in place from the sibling checkout. Every task that
reaches the guest runs inside `scripts/vm.sh exec`, which puts the guest's
`ssh` and `scp` first on `PATH` and keeps any other run off the guest
until it finishes:

```sh
chore matrix -- smoke            # build the CLI if it changed, start the VM, run, fetch evidence
chore matrix -- <scenario filter>
chore vm:status                  # what the guest is doing
chore vm:up / vm:down            # start it (a running guest is reused) / clean shutdown
chore vm:setup                   # provision this crate's tools on the guest
```

After a run, `test-diagnostics/matrix/<scenario>/vm/` holds what Windows
wrote (chkdsk's reports, enumerations), and `summary.txt` says per
scenario what chkdsk found.

Agent coordination rules: see
[`docs/multi-agent-test-protocol.md`](docs/multi-agent-test-protocol.md)
(historical — describes the v1 matrix flow; the claim / update / reset
state machine it describes now lives in `../fs-windows-test-harness/scripts/`).

### Pre-commit hooks

One-time per clone — formats with `cargo fmt` and runs the same
`cargo clippy --all-targets -- -D warnings` check CI runs. The hooks are
[github-guard](https://github.com/antimatter-studios/agent-skills)'s, installed
into `.git/hooks`, where a branch checkout cannot replace them:

```sh
~/.claude/skills/github-guard/install.sh .
```

Bypass a single commit with `git commit --no-verify`.

## Using from C

Link `libfs_ntfs.a` and include `fs_ntfs.h`:

```c
#include "fs_ntfs.h"

fs_ntfs_fs_t *fs = fs_ntfs_mount("/path/to/ntfs.img");
if (!fs) {
    fprintf(stderr, "mount: %s\n", fs_ntfs_last_error());
    return 1;
}

fs_ntfs_attr_t attr;
if (fs_ntfs_stat(fs, "/readme.txt", &attr) == 0) {
    printf("size=%llu\n", (unsigned long long)attr.size);
}

fs_ntfs_umount(fs);
```

Callback-based mount for hosts that can't open the device directly
(sandboxed FSKit extensions, out-of-process backends):

```c
static int my_read(void *ctx, void *buf, uint64_t off, uint64_t len) { /* … */ }

fs_ntfs_blockdev_cfg_t cfg = {
    .read       = my_read,
    .context    = my_context,
    .size_bytes = total_bytes,
    /* .write left NULL for a read-only mount */
};

fs_ntfs_fs_t *fs = fs_ntfs_mount_with_callbacks(&cfg);
```

End-to-end fsck against a callback-held block device, with progress:

```c
static int on_progress(void *ctx, const char *phase,
                       uint64_t done, uint64_t total) { /* … */ return 0; }

uint64_t bytes = 0;
uint8_t  cleared = 0;
int rc = fs_ntfs_fsck_with_callbacks(&cfg, on_progress, NULL,
                                     &bytes, &cleared);
```

Full ABI surface: `include/fs_ntfs.h`. Implementation entry point:
`src/lib.rs`.

## Using from Rust

The C ABI is the primary surface. To use fs-ntfs from Rust — read
*or* write — pull the crate in as a path or git dependency and use
the modules under `facade::` and `write::` (read and write both run
through the crate's own in-house implementation; there's no need to
reach for a separate parser). Note that the Rust facade re-parses
the boot sector and MFT per call (it's stateless by design); for hot
paths, go through the C ABI which keeps the parsed volume in memory.

## Credits

This crate originally built on [Colin Finck](https://github.com/ColinFinck)'s
read-only [`ntfs`](https://github.com/ColinFinck/ntfs) parser
(MIT/Apache-2.0). The read path has since been reimplemented
in-house and supersedes it; the read, write, recovery, mkfs, and FFI
code here is all original work. Colin Finck's crate remains as a
**dev-dependency oracle** — tests cross-check that what this crate
parses and generates matches what the upstream parser sees — but it
is not part of the shipping library. Format evidence combines published
Microsoft structure documentation with Windows-written fixtures and
independent readers. Each citation must support the particular structure
or field it accompanies; see the [source policy](AGENTS.md#source-material-and-format-evidence).

## Disclaimer — use at your own risk

**Read this before pointing the crate at anything you care about.**

This is experimental filesystem code that reads *and writes* the
on-disk structures of live filesystems. Bugs in this class of code
can — and sooner or later will — corrupt or destroy data. The
Apache-2.0 / MIT license above already contains the standard
no-warranty and limitation-of-liability clauses; this section
restates them in plain English so there is no ambiguity about what
you are agreeing to when you use the software.

**By using this software you accept that:**

- The author(s) and contributors provide this crate **as is**, with
  **no warranty of any kind**, express or implied — including but
  not limited to warranties of merchantability, fitness for a
  particular purpose, correctness, data integrity, durability,
  security, or non-infringement.
- The author(s) and contributors are **not liable** for any loss,
  damage, or expense of any kind arising out of or related to your
  use of the software. This explicitly includes (non-exhaustively)
  lost or corrupted data, corrupted filesystems, volumes that will
  no longer mount, hardware damage, downtime, lost revenue, missed
  deadlines, support costs, or any direct, indirect, incidental,
  special, consequential, or punitive damages — regardless of the
  legal theory under which such damages might be sought.
- You are **solely responsible** for backing up any data that could
  be touched by this software *before* running it. The only safe
  workflow when experimenting with an unofficial filesystem driver
  is: work on disk *images* or on *copies*, never on your only
  copy of anything irreplaceable.
- If that is not acceptable to you, **do not use this software**.

This disclaimer is a plain-English restatement of the license terms
above, not a separate license. The license terms apply in full.
