#![no_main]
//! The structure-aware explorer (#483): a seed volume, its records and
//! index blocks mutated field by field with their fixups kept valid, read,
//! written to and read again. The input is the recipe; see
//! `fuzz/shared/structured.rs`.
use fs_ntfs_fuzz::structured;
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

fuzz_target!(|recipe: &[u8]| {
    static SEEDS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    let seeds = SEEDS.get_or_init(|| {
        structured::volumes(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/image"))
    });
    structured::explore(seeds, recipe);
});
