//! `mkfs.ntfs` — format a device or image as NTFS.
//!
//! The same formatter as `rust-ntfs format`, under the name the
//! filesystem tooling convention uses. `am-fs-ext4` ships `mkfs.ext4`
//! and `am-fs-erofs` ships `mkfs.erofs`; NTFS only offered the
//! subcommand, so a caller reaching for the obvious name found
//! nothing.
//!
//! SHARES THE IMPLEMENTATION RATHER THAN COPYING IT. `#[path]` pulls in
//! the very file the subcommand uses, so the two cannot drift: one
//! argument parser, one set of defaults, one set of error messages. A
//! second implementation of a formatter is exactly the kind of copy
//! that ends up quietly weaker than the original.
//!
//! The target name is `mkfs_ntfs` because cargo will not accept a dot in
//! one. The release renames it: `scripts/package-cli.sh` packages it as
//! `mkfs.ntfs` in the per-platform tarballs `release.yml` publishes on the
//! tag's GitHub release, which is what a package manager installs. A
//! `cargo install am-fs-ntfs` still puts `mkfs_ntfs` on the path, and
//! installing it under the conventional name is then the installer's step:
//!
//! ```text
//! install -m755 target/release/mkfs_ntfs /usr/local/sbin/mkfs.ntfs
//! ```
//!
//! What the binary CALLS itself is already conventional — usage and
//! errors say `mkfs.ntfs`, because that is the name passed to
//! `format::run` below, and a tool invoked through a renamed link should
//! not contradict the name it was invoked by.

use std::process::ExitCode;

#[path = "rust_ntfs/format.rs"]
mod format;

fn main() -> ExitCode {
    // Skip argv[0]: the shared parser expects the arguments that follow
    // the subcommand, and here there is no subcommand to skip past.
    let args: Vec<String> = std::env::args().skip(1).collect();
    format::run(args, "mkfs.ntfs")
}
