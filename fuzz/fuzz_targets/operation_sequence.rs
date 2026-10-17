#![no_main]

// Compile the exact gate harness, including its independent parser/oracles.
#[allow(dead_code)]
#[path = "../../tests/op_sequence_readback.rs"]
mod readback;

libfuzzer_sys::fuzz_target!(|input: &[u8]| readback::fuzz_input(input));
