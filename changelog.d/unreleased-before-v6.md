### Added

- **`docs/features.md`, a features page kept current by every pull request.**
  Each feature's state, the release it shipped in, its tracking issue and the
  test that checks it. The README's status, feature, "What works", "What
  doesn't work" and roadmap sections, which still called LZNT1 reads,
  attribute lists and overflowed-directory writes missing, are a short
  summary pointing to it.
- **A `$LogFile` holding `UpdateRecordDataRoot` redo is replayed** (#137).
  With quota tracking on, NTFS charges every allocation to the owner's
  `$Quota` entry in place and logs it as this operation. `fsck` and every
  read-write mount refused any log holding one; they now overwrite the
  entry's data, and leave the record's LSN where it was, as Windows does.
  A volume Windows left mid-write with quotas on, new in `test-disks/`,
  replays to what Windows' own restart produced from it.
- **The `$LogFile` capture has a workload that touches the view indexes.**
  `logfile-oracle.yml`'s `mix: views` turns quota tracking on and gives
  every file its own security descriptor, some an object id or a junction,
  and truncates some, so captures can hold the redo operations replay still
  refuses because no capture has held them (#137).

### Changed

- **`cargo install` puts only `rust-fs-ntfs` on PATH.** `rust-ntfs`, the
  Windows test matrix's driver, is built only with the test-only `harness`
  feature, so neither `cargo install rust-fs-ntfs` nor `--features cli`
  installs it beside the tools. The matrix, the test tiers and clippy build
  with the feature, and `tests/one_binary_on_path.rs` holds the manifest to
  one installed binary (#452).
- **The family's scripts run in place from rust-fs-core, and this repository
  keeps no copy.** `scripts/core.sh` and `scripts/tier.sh` are gone; CI and
  chores run `../rust-fs-core/scripts/NAME.sh` at the pinned version
  (rust-fs-core 0.3.3, #212).
- **The Windows jobs build the VHD writer from its own checkout, at
  rust-img-vhd v0.6.1, with `--locked`.** No `rust-fs-core` is cloned beside it
  any more, so the writer is built against the core it was tested with, not
  the one this repository pins (#462).
- **The tier budgets are a data file.** The measured table that lived inside
  the old `scripts/tier.sh` is `scripts/tier-budgets.txt`, with its
  measurements, and rust-fs-core's runner reads it (`tier.sh --refuse-skips
  TIER -- COMMAND`).
- **A release's notes are its CHANGELOG section**, and a tag the CHANGELOG
  does not describe stops before anything is published (rust-fs-core#209).

### Fixed

- **A rename whose new name sorts into another slot of the directory index
  moves the entry there** (#466). A same-length rename wrote the new name
  over the old one in place, so `d` renamed to `a` beside `b` and `c` sat
  after `c`, where no lookup -- this crate's, ntfs-3g's or the kernel's --
  looks: the rename returned `Ok` and the file could no longer be found by
  name. The entry is now taken out and inserted again, keeping its times,
  sizes and flags, and each step is undone if a later one fails. An entry
  in an interior node of the index is refused instead, as an unlink of one
  is (#495), with nothing written.
- **A write with no read-write mount before it replays the log first, or
  is refused** (#137). `Filesystem::mount` followed by a mutator,
  `write::create_file` and every other path-based mutator in `write`, and
  `fs_ntfs_create_file` and the other C ABI functions that take an image
  path, wrote without the check a read-write mount runs. On a volume
  Windows left mid-write, the new file took an MFT record Windows' restart
  gives to a file of its own, so when Windows later replayed its log one of
  the two was lost. Each now replays a log holding work first, and refuses
  a dirty volume or a log it cannot replay in full, with nothing written.
- **Three on-disk fields are multiplied only by checked arithmetic.** A
  boot sector's `total_sectors` (#475), `$MFT:$Bitmap`'s value length
  (#476) and a log record's `target_vcn` in replay (#507) were multiplied
  raw: a panic in a debug build, and in release a wrapped product that sized
  the volume wrongly, let `$MFT` hand out a record in use, or named the wrong
  record for a replayed block. Each overflow is now a refusal, and
  `$MFT:$Bitmap`'s length is first bounded by the clusters its runs hold.
- **Header fields other fields depend on are checked before they are
  trusted.** An MFT record whose `bytes_allocated` is not the record size is
  refused, as ntfs-3g refuses it, rather than served (#509). A boot sector
  whose index block size is not a power of two between one sector and 64 KiB
  is refused, so `mkdir` no longer builds directories this crate cannot read
  back (#508). An INDX insert bounds every entry by the header's
  `index_length`, and refuses rather than slicing past it (#477).
- **A transient HTTP 5xx from the chore release download no longer fails a CI
  job.** Every chore download in `ci.yml`, `fuzz.yml` and `release.yml`
  retries up to five times on any error.
- **A `$LogFile` whose checkpoint names no start is replayed** (#137). The
  first checkpoint NTFS writes after its log was emptied (by `chkdsk /L`,
  say) has nothing before it to start from: its start of checkpoint is 0
  and it dumps no tables. Replay read that 0 as an LSN and refused the log;
  analysis now starts at the checkpoint itself, as Windows' restart does on
  the volume new in `test-disks/`.
- **`chore test:scripts` passes under a developer's own `.test-env`.**
  chore exports that file into every task, and `run-matrix-test-env.sh`
  passed its `HOST_IMAGE_DIR` (the README suggests `/tmp`) on to the
  sandboxed run, which failed the case that names none. The test clears the
  developer's settings, and checks that nothing reaches them.
