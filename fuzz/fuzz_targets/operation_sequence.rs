#![no_main]

// Compile the exact stable runner, including its independent parser/oracles.
#[allow(dead_code)]
#[path = "../../tests/op_sequence_model/runner.rs"]
mod runner;

libfuzzer_sys::fuzz_target!(|input: &[u8]| runner::fuzz_input(input));
