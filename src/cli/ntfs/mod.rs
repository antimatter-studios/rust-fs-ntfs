//! The NTFS tools: what this repository fills the shared contract in
//! with. Everything filesystem-specific lives here, and nothing here is
//! plumbing (that is `fs_core::cli`, in am-fs-core).

pub mod device;
pub mod fs;
pub mod fsck;
pub mod mkfs;

// THE FORMATTER'S ONE IMPLEMENTATION, shared with `rust-ntfs format`, the
// test matrix's driver. `mkfs.ntfs` parses its command line with clap and
// hands the result to the same `execute`; the hand parser in that file is
// `rust-ntfs`'s and is not reached from here.
#[path = "../../bin/rust_ntfs/format.rs"]
#[allow(dead_code)]
pub mod format;
