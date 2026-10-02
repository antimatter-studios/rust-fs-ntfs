//! `rust-fs-ntfs`: the command-line tools for NTFS, one multi-call binary.
//!
//! Installed as `rust-fs-ntfs` and linked as each dotted name. The
//! dispatch and the output contract every tool shares are `fs_core::cli`
//! (am-fs-core's `cli` feature); `ntfs` is the tools themselves.

mod ntfs;
#[cfg(test)]
#[path = "../test_scratch.rs"]
mod test_scratch;

use fs_core::cli;
use std::process::ExitCode;

static FAMILY: cli::Family = cli::Family {
    repo: "rust-fs-ntfs",
    crate_name: env!("CARGO_PKG_NAME"),
    version: env!("CARGO_PKG_VERSION"),
    about: "NTFS tools: work on an NTFS image or device directly, without mounting it",
    install_hints: &[
        "`chore cli:install` from a checkout of this repository",
        "`brew install antimatter-studios/tap/rust-fs-ntfs`",
    ],
    tools: &[ntfs::mkfs::TOOL, ntfs::fsck::TOOL, ntfs::fs::TOOL],
};

fn main() -> ExitCode {
    cli::main(&FAMILY)
}
