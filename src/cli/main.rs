//! `rust-fs-ntfs`: the command-line tools for NTFS, one multi-call binary.
//!
//! Installed as `rust-fs-ntfs` and linked as each dotted name; see
//! `common` for the dispatch and the output contract every tool shares,
//! and `ntfs` for the tools themselves.

// The shared plumbing is a library in waiting (see its module docs): its
// API is whole, and a piece NTFS does not call yet is not dead, it is the
// part another driver's tools will.
#[allow(dead_code)]
mod common;
mod ntfs;

use std::process::ExitCode;

static FAMILY: common::Family = common::Family {
    repo: "rust-fs-ntfs",
    crate_name: env!("CARGO_PKG_NAME"),
    version: env!("CARGO_PKG_VERSION"),
    about: "NTFS tools: work on an NTFS image or device directly, without mounting it",
    install_hints: &[
        "`chore cli:install` from a checkout of this repository",
        "`brew install antimatter-studios/tap/rust-fs-ntfs`",
    ],
    tools: &[ntfs::mkfs::TOOL, ntfs::fs::TOOL],
};

fn main() -> ExitCode {
    common::main(&FAMILY)
}
