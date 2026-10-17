//! Minimize a failing operation sequence into a pinned corpus regression.
//! Run from the repository root; tests/op_sequence_model/README.md explains
//! the workflow. INPUT is a JSON replay or raw generator bytes, and is only
//! read; NAME describes the finding in lowercase words and hyphens.
#[allow(dead_code)]
#[path = "../tests/op_sequence_readback.rs"]
mod readback;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, name] = &args[..] else {
        eprintln!("usage: cargo run --release --example op_sequence_capture -- INPUT NAME");
        return ExitCode::from(2);
    };
    match readback::capture(Path::new(input), name) {
        Ok(pinned) => {
            println!("{}", pinned.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("op_sequence_capture: {error}");
            ExitCode::FAILURE
        }
    }
}
