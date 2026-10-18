//! The operation executor shared by stable corpus mutation and libFuzzer.
#[path = "../common/mod.rs"]
pub(crate) mod common;
#[allow(dead_code)]
#[path = "generator.rs"]
pub(crate) mod generator;
#[allow(dead_code)]
#[path = "harness.rs"]
pub(crate) mod harness;
#[path = "model.rs"]
pub(crate) mod model;
#[path = "shrink.rs"]
pub(crate) mod shrink;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub(crate) const SIZE: u64 = 64 * 1024 * 1024;
/// The committed seeds and captured regressions, replayed by the gate.
pub(crate) const CORPUS: &str = "fuzz/corpus/operation_sequence";

pub(crate) fn reference_tools(image: &str, model: &BTreeMap<String, Vec<u8>>, artifacts: &Path) {
    let fixed = std::process::Command::new("ntfsfix")
        .args(["-n", image])
        .output()
        .expect("ntfsfix missing: install ntfs-3g for test:op-sequence-oracle");
    let mut report = fixed.stdout;
    report.extend_from_slice(&fixed.stderr);
    fs::write(artifacts.join("ntfsfix.txt"), &report).expect("save oracle verdict");
    assert!(
        fixed.status.success(),
        "ntfsfix -n: {}",
        String::from_utf8_lossy(&report)
    );
    for (index, (path, expected)) in model.iter().enumerate() {
        let output = std::process::Command::new("ntfscat")
            .args([image, path])
            .output()
            .expect("ntfscat missing: install ntfs-3g for test:op-sequence-oracle");
        fs::write(
            artifacts.join(format!("ntfscat-{index}.bin")),
            &output.stdout,
        )
        .unwrap();
        fs::write(
            artifacts.join(format!("ntfscat-{index}.txt")),
            &output.stderr,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "ntfscat {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(&output.stdout, expected, "ntfs-3g contents of {path}");
    }
}

/// The capture command (#684), run from the repository root:
/// `cargo run --release --example op_sequence_capture -- INPUT NAME`.
#[allow(dead_code)]
pub fn capture(original: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    shrink::quiet_panics();
    harness::capture(original, name, Path::new(CORPUS), |model| model)
}

/// Valid JSON replays concretely; other bytes use the live generator.
#[allow(dead_code)]
pub fn fuzz_input(input: &[u8]) {
    // Raw bytes choose the image they run on (#1024), a pin replays on its
    // own, and geometry.txt beside every finding names it.
    let (replayed, geometry, rest) = match generator::decode_regression(input) {
        Ok((operations, pinned)) => (
            Some(operations),
            pinned.map_or_else(harness::geometry, |pinned| pinned.geometry),
            input,
        ),
        Err(_) => {
            let (geometry, rest) = generator::decode_geometry(input);
            (None, geometry, rest)
        }
    };
    let mut h = harness::Harness::on("explorer", &geometry);
    fs::write("tmp/op-sequence-model/explorer/input.bin", input).unwrap();
    match replayed {
        Some(operations) => {
            for operation in operations {
                h.step(operation);
            }
        }
        None => {
            h.generated(rest);
        }
    }
    h.finish();
    // Keep totals across completed inputs as well as the current prefix's
    // per-opcode accepted/refused counts when a later input crashes.
    use generator::OPCODES;
    static TOTALS: std::sync::Mutex<harness::Counts> =
        std::sync::Mutex::new(([0; OPCODES], [0; OPCODES]));
    let mut totals = TOTALS.lock().unwrap();
    for kind in 0..OPCODES {
        totals.0[kind] += h.counts.0[kind];
        totals.1[kind] += h.counts.1[kind];
    }
    fs::write(
        "tmp/op-sequence-model/explorer/refusal-histogram.json",
        serde_json::to_vec(&*totals).unwrap(),
    )
    .unwrap();
}
