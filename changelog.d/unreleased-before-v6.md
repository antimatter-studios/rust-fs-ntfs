### Breaking

The next release is **0.10.0**, not 0.9.1: the change below breaks code
written against 0.9.0, and `chore check:semver` refuses it otherwise.

- **`fsck::CheckFinding` has six new variants, and `fsck::CheckReport` a
  new public field, `clusters_scanned`** (#517): what `check_io` finds when
  it accounts for every cluster against `$Bitmap`. A `match` on
  `CheckFinding` without a wildcard arm, or a `CheckReport` literal naming
  every field, no longer compiles.

### Added

- **`fs.ntfs rm` and `fs.ntfs rmdir`** (#550). The namespace verbs the CLI
  design names, removing a file and an empty directory. A matrix scenario
  in CI's Windows job fills two directories past one index level, empties
  them with these verbs, and has Windows read the survivors back and
  `chkdsk` find the volume clean.
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

- **The Windows test VM is fs-windows-test-harness's QEMU guest, and the
  VMware driver is gone.** `HARNESS_REF` moves to v4.4.0, whose persistent
  Windows ARM64 guest runs under QEMU on Linux ARM64 (KVM, a Raspberry Pi 5
  among them) and on Apple Silicon (HVF). `chore vm:up`, `vm:status` and
  `vm:down` run the harness's `scripts/local-vm.py` in place, `chore
  vm:setup` provisions the guest, and every task that reaches it runs inside
  `scripts/vm.sh exec`, where `ssh` and `scp` are the guest's and no other
  run can use it at the same time. `vmrun`, the trove password and SSH
  agent, `VM_VMX`, `VM_STATIC_IP`, `vm:address` and `vm:network` are
  removed. `.test-env` names `VM_HOST=fswth-local`, an empty `SSH_KEY`, and
  `VM_STATE` for a guest outside the harness's default state directory.
- **`chore vm:setup` provisions the QEMU guest end to end.** Measured on it,
  `scripts/setup-windows-vm.ps1` needed five changes:
  - The guest's Windows 11 IoT Enterprise LTSC has no Microsoft Store, so
    no winget. It is installed from winget-cli v1.29.380's release, every
    file pinned by SHA-256. winget's catalogue is provisioned the same way,
    because over SSH winget's own per-user install of it fails with
    `0x80073D19` ("a user was logged off").
  - Git, which clones `rust-img-vhd`, comes through winget too.
  - The test work paths are excluded from Defender's real-time scanning:
    with them scanned, LLVM-MinGW's ~9,000 files unpacked at 1.6 a second,
    against 18 a second without.
  - `rust-img-vhd` is built at `IMG_VHD_REF`, as CI builds it, with
    `--locked`, and with rustup's default host set to gnullvm, so its
    `rust-toolchain.toml` does not pick MSVC. It used to build the default
    branch, which needed a `rust-fs-core` checkout beside it that was never
    there.
  - A failed clone or `cargo install` stops the setup. It printed "Setup
    complete" over both.
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

- **`check_io` finds a record whose attributes are malformed** (#515). It
  read each in-use record's header and stopped there, so a resident value
  past its attribute, attributes out of type order, an id the header never
  handed out, a chain not ending where the header says, or a run list that
  disagrees with its VCNs was reported clean while this crate's own reader
  and ntfs-3g refused the file. It now walks each record's attributes and
  reports a bad record; the volumes Windows wrote stay clean.
- **A name is created exactly when Windows would create it** (#498).
  `NUL`, in any case, was created as a file, and a Win32 program then opens
  the NUL device under that name. Create, mkdir and rename now refuse it
  with `EINVAL`. The rule follows Windows rather than Microsoft's naming
  page: measured on Windows 10.0.26100, `CON`, `COM1`, `LPT1` and `nul.txt`
  become files through a full path, so they stay allowed. CI measures the
  running Windows' verdicts and grades the crate against them.
- **An EA name is stored as Windows stores it** (#482). `write_ea` kept the
  caller's case, and NTFS stores a name set through Windows' `NtSetEaFile`
  upper-cased: Windows, given `user.a`, stores `USER.A`. Names are now
  stored upper-cased, so `list_eas` and `fs_ntfs_list_ea_keys` report
  `USER.A` for an EA written as `user.a`; lookups were and stay
  case-insensitive. A Windows-authored fixture grades it.
- **A rename whose new name sorts into another slot of the directory index
  moves the entry there** (#466). A same-length rename wrote the new name
  over the old one in place, so `d` renamed to `a` beside `b` and `c` sat
  after `c`, where no lookup -- this crate's, ntfs-3g's or the kernel's --
  looks: the rename returned `Ok` and the file could no longer be found by
  name. The entry is now taken out and inserted again, keeping its times,
  sizes and flags, and each step is undone if a later one fails. An entry
  in an interior node of the index is refused instead, as an unlink of one
  is (#495), with nothing written.
- **Stream names and file attributes Windows refuses are refused.** A named
  stream's name passes the same checks as a file name, so `a:b`, `x:$DATA`, a
  NUL inside, or a trailing space or period is refused rather than written
  where no Windows API can reach it (#497). `set_file_attributes` changes
  only the bits `SetFileAttributes` can, and refuses DIRECTORY, SPARSE_FILE,
  REPARSE_POINT, COMPRESSED, ENCRYPTED and the reserved bits (#502).
- **A `$DATA` run list split across MFT records is read** (#473). A file
  whose mapping pairs continue in an extension record -- a heavily
  fragmented file on a long-lived Windows volume -- was refused by every
  read and by `stat`. Its segments are now stitched together in VCN order,
  and segments that leave a gap or end short of the allocated size are
  still refused.
- **`fsck.ntfs` accounts for every cluster** (#517). `check_io` never
  compared `$Bitmap` with the clusters files use, so a volume with leaked
  clusters, clusters a file holds marked free, two files sharing a run, or a
  run past the volume's end was reported clean. It now collects the run
  list of every non-resident attribute in every in-use MFT record and
  reports each of the four by kind, with `cluster_allocation` among the
  report's `checks` and the clusters compared under `scanned`. Over a log
  that holds work the comparison waits for the replay, since `$Bitmap` and
  the run lists disagree until then. The features page no longer says
  `check_io` checks every index: it checks none (#515). `CheckFinding` has
  six new variants and `CheckReport` a `clusters_scanned` field.
- **A write with no read-write mount before it replays the log first, or
  is refused** (#137). `Filesystem::mount` followed by a mutator,
  `write::create_file` and every other path-based mutator in `write`, and
  `fs_ntfs_create_file` and the other C ABI functions that take an image
  path, wrote without the check a read-write mount runs. On a volume
  Windows left mid-write, the new file took an MFT record Windows' restart
  gives to a file of its own, so when Windows later replayed its log one of
  the two was lost. Each now replays a log holding work first, and refuses
  a dirty volume or a log it cannot replay in full, with nothing written.
- **Every write to a metafile is refused before anything is touched (#499).**
  Records 0 to 23 other than the root, and `$Extend`'s children, are refused
  with `EPERM` by every mutating entry point: unlink, remove, rename (as
  source or as the name replaced), truncate, grow, every content, stream,
  EA, reparse, object id, attribute, time and security write, link, and a
  create or mkdir into `$Extend`. `$Volume` and `$BadClus` could be
  unlinked, `$UpCase` truncated and every metafile renamed, and an unlink
  of `$MFT` was refused only after the record and index had changed.
- **A host's directory listing leaves the NTFS metafiles out** (#499).
  `Filesystem::read_dir` and `fs_ntfs_dir_open` listed `$MFT`, `$LogFile`,
  `$Volume` and the rest, which Windows does not enumerate and ntfs-3g
  hides. Both now leave records 0 to 15 out, as the CLI's `ls` does; a
  metafile is still found by name.
- **A new file is never given one of the reserved records 16 to 23.** The
  search for a free record wrapped round to 0 once every record from 24 up
  was in use, so on a volume whose `$MFT:$Bitmap` leaves them clear, as
  Windows' does, a file landed in record 16. It now grows `$MFT` instead,
  as Windows and ntfs-3g do.
- **Named streams are matched by name under `$UpCase`** (#494). Writing
  `s1` to a file holding `S1` made a second stream, where Windows treats
  the two as one. Writes, deletes and reads now address the stream whose
  stored name collates equal, and keep its spelling.
- **Every write to a metafile is refused before anything is touched (#499).**
  Records 0 to 23 other than the root, and `$Extend`'s children, are refused
  with `EPERM` by every mutating entry point: unlink, remove, rename (as
  source or as the name replaced), truncate, grow, every content, stream,
  EA, reparse, object id, attribute, time and security write, link, and a
  create or mkdir into `$Extend`. `$Volume` and `$BadClus` could be
  unlinked, `$UpCase` truncated and every metafile renamed, and an unlink
  of `$MFT` was refused only after the record and index had changed.
- **Writes advance the times of what they change** (#493). No write
  touched `$STANDARD_INFORMATION`'s times, so a rewritten file, or a
  directory a file was created in, looked unchanged to backup and sync
  tools. Path-based writes now stamp the file's modified and change times,
  entry changes stamp the directory's, and a rename or link stamps the
  file's change time; the record-number primitives leave times to the host.
- **Three on-disk fields are multiplied only by checked arithmetic.** A
  boot sector's `total_sectors` (#475), `$MFT:$Bitmap`'s value length
  (#476) and a log record's `target_vcn` in replay (#507) were multiplied
  raw: a panic in a debug build, and in release a wrapped product that sized
  the volume wrongly, let `$MFT` hand out a record in use, or named the wrong
  record for a replayed block. Each overflow is now a refusal, and
  `$MFT:$Bitmap`'s length is first bounded by the clusters its runs hold.
- **A directory whose `$INDEX_ROOT` is in an extension record can be read**
  (#471). The directory readers took `$INDEX_ROOT` from the base record
  only, so a directory whose attributes outgrew it -- one long name, or
  many hard links -- answered "$INDEX_ROOT:$I30 not found" to a listing and
  to every lookup inside it. They now take the index attributes from
  wherever `$ATTRIBUTE_LIST` puts them.
- **A directory entry carries the size of the file it names** (#506). Every
  `$I30` entry duplicates the file's data size and allocated size, and
  Windows lists a directory from that copy. It was written at create time
  and never again, so every file this crate wrote listed on Windows as 0
  bytes. Each path-based write that changes a file's size now refreshes the
  entry for every name the file has. A host writing by record number calls
  the new `refresh_directory_entry_sizes_io` when it is done with the file,
  as Windows refreshes on the last close.
- **Positional writes start, grow and extend a file** (#500). `write_at`
  and `fs_ntfs_write_file` took only a non-resident `$DATA` inside its
  length, so the first write to every new file, every write to a small one
  and every append failed, and `truncate` and `grow` refused resident
  files. A resident value is now spliced and written back, promoted when it
  outgrows the record, and a non-resident one grown to the write's end,
  leaving any gap as zeros.
- **A path that differs in case from the stored name renames the stored
  name** (#491). Rename looked the record's `$FILE_NAME` up by the caller's
  spelling, so renaming `FILE.TXT` through `/d/file.txt` left the record
  saying `FILE.TXT` under a renamed index entry, a file the Linux kernel
  cannot open. A case-only rename spelled like its target did nothing. The
  name as stored is used throughout.
- **A refusal names the operation that was refused** (#501). `rmdir`,
  `unlink` and `remove` of the root said "cannot rename root", and
  unlinking a directory said rmdir was not implemented. Each now names its
  own operation, and the unlink refusal points at `rmdir` as it is.
- **A directory past one index level can be emptied and removed** (#495).
  An entry of an interior index node could not be unlinked, and rmdir
  refused any directory with `$INDEX_ALLOCATION`, so such a directory could
  never be emptied nor removed. An interior entry is now replaced by its
  in-order neighbour, as a B-tree does, and rmdir removes a directory whose
  every index block is empty and gives their clusters back.
- **Header fields other fields depend on are checked before they are
  trusted.** An MFT record whose `bytes_allocated` is not the record size is
  refused, as ntfs-3g refuses it, rather than served (#509). A boot sector
  whose index block size is not a power of two between one sector and 64 KiB
  is refused, so `mkdir` no longer builds directories this crate cannot read
  back (#508). An INDX insert bounds every entry by the header's
  `index_length`, and refuses rather than slicing past it (#477).
- **Named streams are inserted in collation order (#467).** Within a type, a
  new attribute goes where its name sorts, case-insensitively under the
  volume's `$UpCase` and then case-sensitively, rather than after every other
  of its type. ntfs-3g and Windows stop looking once they pass an attribute's
  slot, so a stream written after one that sorts later was one they could
  not find.
- **A record with an `$ATTRIBUTE_LIST` gets no attribute added or removed**
  (#468). The list names every attribute a file has and where, and this
  driver does not maintain it: a stream added to such a file went into the
  base record with no list entry, unreadable by ntfs-3g, while the write
  returned `Ok`. Adding or removing an attribute on such a record is now
  refused with `EPERM` before anything is written; a rewrite in place still
  works.
- **A symlink is written in Windows path syntax and read back in POSIX
  syntax** (#505). Targets were stored verbatim, with `/` separators and a
  POSIX absolute path marked absolute, which neither Windows nor ntfs-3g
  can resolve; and a Windows link's relative target came back with `\`
  separators. Targets are written as Windows writes them -- `\??\X:\…`
  for a drive, relative otherwise, a `/`-rooted target as a path from the
  link's volume root -- and a relative target reads back with `/`.
- **A transient HTTP 5xx from the chore release download no longer fails a CI
  job.** Every chore download in `ci.yml`, `fuzz.yml` and `release.yml`
  retries up to five times on any error.
- **A rename gives the new name the namespace create would** (#492). A
  rename kept the old name's `$FILE_NAME` namespace, so `file.txt` renamed
  to `longer_name.txt` stayed `WIN32_AND_DOS`, which chkdsk rejects on a
  name that does not fit 8.3. The record and the index entry now both take
  POSIX, or WIN32_AND_DOS for an 8.3 name; one half of a WIN32 + DOS pair
  keeps its namespace.
- **A file's contents can be replaced again once they are non-resident**
  (#470). `write_file_contents` promoted a file that outgrew its record,
  and then refused every later rewrite of it with "$DATA is already
  non-resident": the second save of any file over about 700 bytes failed.
  A non-resident file is now resized in place and written over.
- **A `$LogFile` whose checkpoint names no start is replayed** (#137). The
  first checkpoint NTFS writes after its log was emptied (by `chkdsk /L`,
  say) has nothing before it to start from: its start of checkpoint is 0
  and it dumps no tables. Replay read that 0 as an LSN and refused the log;
  analysis now starts at the checkpoint itself, as Windows' restart does on
  the volume new in `test-disks/`.
- **A refused named-stream rewrite leaves the stream as it was** (#469).
  When the new contents had to go non-resident, the existing stream was
  deleted first, so a rewrite refused for want of record space or of free
  clusters returned `Err` with the stream already gone. The stream is now
  replaced in place, and its old clusters are freed only after the new
  attribute is written.
- **Listing a file's named streams follows its `$ATTRIBUTE_LIST`** (#472).
  Only the base record was walked, so a file whose streams spilled into
  extension records listed some of them, with no error. The listing now
  names every stream the attribute list holds, each once.
- **`chore test:scripts` passes under a developer's own `.test-env`.**
  chore exports that file into every task, and `run-matrix-test-env.sh`
  passed its `HOST_IMAGE_DIR` (the README suggests `/tmp`) on to the
  sandboxed run, which failed the case that names none. The test clears the
  developer's settings, and checks that nothing reaches them.
- **A rename to another length works in a directory with INDX blocks**
  (#474). It was refused whenever the parent had `$INDEX_ALLOCATION`, which
  the root of every new volume has. The entry is now taken out of the index
  and the new one inserted where it sorts, each step undone if a later one
  fails.
- **`set_security_id` refuses an id with no descriptor in `$Secure:$SII`**
  (#503). Any 32-bit value was written, and Windows applies no descriptor
  to a file whose id `$SII` does not hold. Such an id is now refused with
  `InvalidInput`, before anything is written.
