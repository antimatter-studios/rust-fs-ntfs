//! `cargo install rust-fs-ntfs --features cli` puts exactly one program on
//! PATH: `rust-fs-ntfs`, the multi-call binary named for the repository,
//! which answers to `mkfs.ntfs`, `fsck.ntfs` and `fs.ntfs` and which nothing
//! else can shadow. That is also all the release tarball ships.
//!
//! `cargo install` installs every `[[bin]]` whose `required-features` the
//! features it was given meet. `rust-ntfs`, the Windows test matrix's
//! driver, is not a user's tool, so it is built only with the test-only
//! `harness` feature (#452). This reads the manifest and works out what the
//! installs a user would type put on PATH.

use std::collections::{BTreeMap, BTreeSet};

const ENTRY_POINT: &str = "rust-fs-ntfs";

/// Features that exist for the tests alone. No install a user types turns
/// one on, so a binary that requires one is never installed.
const TEST_ONLY: &[&str] = &["harness"];

/// The feature sets of the installs a user types: plain `cargo install`,
/// and the one the README gives.
const INSTALLS: &[&[&str]] = &[&[], &["cli"]];

#[derive(Debug, PartialEq)]
struct Bin {
    name: String,
    required: Vec<String>,
}

/// The items of a one-line TOML array of strings.
fn array(value: &str, line: &str) -> Vec<String> {
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or_else(|| panic!("not a one-line array, which this scan cannot read: {line:?}"));
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Every `[[bin]]` table (name, required features) and the `[features]`
/// table (feature -> what it turns on).
fn scan(manifest: &str) -> (Vec<Bin>, BTreeMap<String, Vec<String>>) {
    #[derive(PartialEq)]
    enum Table {
        Bin,
        Features,
        Other,
    }
    let mut bins: Vec<Bin> = Vec::new();
    let mut features = BTreeMap::new();
    let mut table = Table::Other;
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') && !line.contains('=') {
            table = match line {
                "[[bin]]" => {
                    bins.push(Bin {
                        name: String::new(),
                        required: Vec::new(),
                    });
                    Table::Bin
                }
                "[features]" => Table::Features,
                _ => Table::Other,
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match table {
            Table::Bin => {
                let bin = bins.last_mut().expect("a [[bin]] table is open");
                match key {
                    "name" => bin.name = value.trim_matches('"').to_string(),
                    "required-features" => bin.required = array(value, raw),
                    _ => {}
                }
            }
            Table::Features => {
                features.insert(key.to_string(), array(value, raw));
            }
            Table::Other => {}
        }
    }
    (bins, features)
}

/// The crate's own features an install turns on: `default` and the ones
/// it names, and everything those turn on in turn. `dep:x` and `x/y` name
/// dependencies, not features of this crate.
fn enabled(features: &BTreeMap<String, Vec<String>>, asked: &[&str]) -> BTreeSet<String> {
    let mut on = BTreeSet::new();
    let mut todo: Vec<String> = asked.iter().map(|f| f.to_string()).collect();
    todo.push("default".to_string());
    while let Some(f) = todo.pop() {
        if f.contains(':') || f.contains('/') || !on.insert(f.clone()) {
            continue;
        }
        if let Some(more) = features.get(&f) {
            todo.extend(more.iter().cloned());
        }
    }
    on
}

/// What `cargo install` puts on PATH with these features.
fn installed(bins: &[Bin], on: &BTreeSet<String>) -> Vec<String> {
    bins.iter()
        .filter(|b| b.required.iter().all(|f| on.contains(f)))
        .map(|b| b.name.clone())
        .collect()
}

fn manifest() -> (Vec<Bin>, BTreeMap<String, Vec<String>>) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let (bins, features) = scan(&std::fs::read_to_string(path).expect("read Cargo.toml"));
    assert!(
        bins.iter().any(|b| b.name == ENTRY_POINT),
        "the scan found no [[bin]] named {ENTRY_POINT}, so it is not reading the manifest it \
         guards: {bins:?}"
    );
    (bins, features)
}

#[test]
fn cargo_install_puts_nothing_but_the_entry_point_on_path() {
    let (bins, features) = manifest();
    for asked in INSTALLS {
        let strays: Vec<String> = installed(&bins, &enabled(&features, asked))
            .into_iter()
            .filter(|n| n != ENTRY_POINT)
            .collect();
        assert!(
            strays.is_empty(),
            "`cargo install rust-fs-ntfs` with features {asked:?} puts {strays:?} on PATH beside \
             {ENTRY_POINT}. A tool for the tests goes behind a test-only feature \
             ({TEST_ONLY:?}) through `required-features`."
        );
    }
}

#[test]
fn the_readme_install_puts_the_entry_point_on_path() {
    let (bins, features) = manifest();
    let names = installed(&bins, &enabled(&features, &["cli"]));
    assert_eq!(
        names,
        vec![ENTRY_POINT.to_string()],
        "`cargo install rust-fs-ntfs --features cli` installs these"
    );
}

#[test]
fn every_other_binary_requires_a_test_only_feature() {
    let (bins, features) = manifest();
    for bin in bins.iter().filter(|b| b.name != ENTRY_POINT) {
        assert!(
            bin.required.iter().any(|f| TEST_ONLY.contains(&f.as_str())),
            "[[bin]] {} requires {:?}, none of them test-only ({TEST_ONLY:?})",
            bin.name,
            bin.required
        );
    }
    for asked in INSTALLS {
        let on = enabled(&features, asked);
        for f in TEST_ONLY {
            assert!(
                !on.contains(*f),
                "an install with features {asked:?} turns on the test-only feature {f}"
            );
        }
    }
}

#[test]
fn the_scan_reads_bins_and_features() {
    let manifest = "[package]\nname = \"x\"\n\n[[bin]]\nname = \"a\"\npath = \"a.rs\"\n\n\
                    [[bin]]\nname = \"b\" # a comment\nrequired-features = [\"t\", \"u\"]\n\n\
                    [package.metadata.x]\nname = \"not-a-bin\"\n\n\
                    [features]\nu = [\"dep:y\", \"z/w\", \"v\"]\nv = []\n";
    let (bins, features) = scan(manifest);
    assert_eq!(
        bins,
        vec![
            Bin {
                name: "a".into(),
                required: vec![],
            },
            Bin {
                name: "b".into(),
                required: vec!["t".into(), "u".into()],
            },
        ]
    );
    let on = enabled(&features, &["u"]);
    assert_eq!(
        on,
        ["default", "u", "v"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert_eq!(installed(&bins, &on), vec!["a".to_string()]);
}
