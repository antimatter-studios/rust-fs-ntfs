# ntfs 0.4.0, patched for 512-byte records

This directory is the `ntfs` crate 0.4.0 by Colin Finck, as published on
crates.io (`ntfs-0.4.0.crate`, SHA-256
`26fe2140f3b6eef2648fa331ca7af7652da1bf3bb84f715878bf50f2f8f03c38`, from
upstream commit `3110cafc0440a84f2593b5d3691e0b23b68ccb8f`), licensed MIT OR
Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`). The published `testdata/`,
`img/` and `.github/` directories, `.gitignore`, `.cargo_vcs_info.json` and
`Cargo.toml.orig` are omitted; nothing here builds or reads them. The
published files' CRLF line endings are stored as LF, and the trailing spaces
of three lines of documentation are removed (`CHANGELOG.md`, `README.md`,
`src/lib.rs`). Every other byte is the published crate's, except for the
four changes below: three in code and one build setting.

rust-fs-ntfs uses this crate only as an independent reader in its tests,
through `[patch.crates-io]` in the top-level `Cargo.toml`. It is never linked
into the library or any binary a consumer builds.

## 512-byte records

`src/boot_sector.rs`, `BiosParameterBlock::record_size`: the smallest
accepted negative size exponent is 9 instead of 10.

A boot sector gives the MFT record and index record sizes as one signed
byte. A positive value counts clusters; a negative value `-n` means `2^n`
bytes. A 512-byte record on a volume whose clusters are larger than 512
bytes can only be written as `-9`. Published 0.4.0 refuses that byte with
`InvalidRecordSizeInfo` before reading anything, because exponents below 10
"have never been seen" (rust-fs-ntfs#1296).

512 bytes is the update sequence stride: the crate's fixup already treats a
record as a sequence of 512-byte blocks, independently of the sector size,
and a 512-byte record is one such block. Every record header the crate
parses fits in it. No other check is changed: exponents below 9 and above
12 are still refused, and every structure the crate validates is validated
as before.

## Index blocks smaller than a cluster

`src/structured_values/index_allocation.rs`,
`NtfsIndexAllocation::record_from_vcn`: when an index record is smaller
than a cluster, its VCN is converted to a byte offset in 512-byte units
instead of clusters (rust-fs-ntfs#1298).

An index entry names the index record below it by VCN. Published 0.4.0
always multiplies that VCN by the cluster size. When index records are
smaller than a cluster, the VCN counts 512-byte units: Windows' formatter
declares 4096-byte index blocks on a 64 KiB-cluster volume as 8 such units
in the root's `$INDEX_ROOT`, and ntfs-3g writes that root's second block at
byte 4096 with the VCN 8 in its header. Published 0.4.0 reads VCN 8 there
as byte 524288 and refuses the directory with
`VcnOutOfBoundsInIndexAllocation`. Nothing changes when an index record is
at least a cluster, and the crate's check that the record found carries
the VCN asked for still applies to every record.

## Trait-path calls of binrw's `read_le`

`src/ntfs.rs`, `Ntfs::new`, and the `new` functions of
`src/structured_values/attribute_list.rs`, `file_name.rs`, `object_id.rs`,
`standard_information.rs` and `volume_information.rs`: each of the ten
method-syntax calls `reader.read_le::<T>()` is written
`BinReaderExt::read_le::<T>(reader)` (rust-fs-ntfs#1300).

`std::io::Read` is gaining a `read_le` method on nightly. Under
`nightly-2026-09-01` every method-syntax call on a reader is ambiguous
between that method and binrw's `BinReaderExt::read_le`, and the compiler
warns with `unstable_name_collisions` that it may resolve to the standard
library's once that is stable. The trait path names the method the crate
has always called, so the same function runs with the same type and the
same reader; no lint is allowed and nothing else changes.
`tests/scripts/vendor-ntfs-read-le.sh` fails if a method-syntax call to
binrw's endian readers returns.

## The build setting

`Cargo.toml` gains a `[lints.rust]` table allowing
`mismatched_lifetime_syntaxes`. Cargo caps the lints of a registry
dependency but not of a path dependency, and 0.4.0 predates that lint, so
without it every build of the tests prints four warnings from this crate.
It changes no code.
