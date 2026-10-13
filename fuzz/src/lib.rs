//! Shared setup for the fuzz targets.
//!
//! The volume and the walk live in `fuzz/shared/walk.rs`, which
//! `tests/fuzz_decoders.rs` includes too -- see that file for why it is
//! shared textually rather than as a dependency.

include!("../shared/walk.rs");

/// The structure-aware explorer, in a module of its own because it
/// imports what `walk.rs` does.
pub mod structured {
    include!("../shared/structured.rs");
}
