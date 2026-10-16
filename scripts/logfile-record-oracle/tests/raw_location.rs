//! Raw-location diagnostics, separate from the unchanged positive identity gate.
use std::{fs, io::Write, path::PathBuf, process::Command};

use sha2::{Digest, Sha256};

const SHA: &str = "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb";
const REV: &str = "b88585646801ee2c1e2acf91a078da6ab57af778";
const SOURCE: u64 = 0x1046bb;
const TARGET: u64 = 0x10443c;

#[derive(Clone, Copy)]
struct Identity {
    lsn: u64,
    offset: usize,
    previous: u64,
    undo: u64,
}

fn word(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn header_at(bytes: &[u8], at: usize, identity: Identity) -> bool {
    at == identity.offset
        && at.checked_add(0x30).is_some_and(|end| end <= bytes.len())
        && word(bytes, at) == Some(identity.lsn)
        && word(bytes, at + 8) == Some(identity.previous)
        && word(bytes, at + 16) == Some(identity.undo)
}

// This is a byte-integrity diagnostic for this capture's 4096/512 geometry,
// not a replacement record walker or an authority to accept a missing record.
fn restored_page(page: &[u8]) -> Result<Vec<u8>, &'static str> {
    if page.len() != 4096 || page.get(..4) != Some(b"RCRD") {
        return Err("not a complete captured RCRD page");
    }
    let usa = usize::from(u16::from_le_bytes(page[4..6].try_into().unwrap()));
    let count = usize::from(u16::from_le_bytes(page[6..8].try_into().unwrap()));
    if count != 9 || usa < 8 || usa + count * 2 > 510 {
        return Err("invalid captured USA bounds/count");
    }
    let sequence = &page[usa..usa + 2];
    for sector in 1..count {
        let trailer = sector * 512 - 2;
        if &page[trailer..trailer + 2] != sequence {
            return Err("torn captured sector trailer");
        }
    }
    let mut restored = page.to_vec();
    for sector in 1..count {
        let trailer = sector * 512 - 2;
        restored[trailer..trailer + 2]
            .copy_from_slice(&page[usa + sector * 2..usa + sector * 2 + 2]);
    }
    Ok(restored)
}

#[test]
fn immutable_raw_locations_reject_pointer_and_torn_page_controls() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let evidence = root.join("tmp/logfile-physical-position");
    fs::create_dir_all(&evidence).unwrap();
    let output = Command::new("gzip")
        .arg("-dc")
        .arg(root.join("test-disks/windows-interrupted-logfile-1.bin.gz"))
        .output()
        .expect("install gzip to read the immutable Windows capture");
    assert!(output.status.success(), "gzip: {:?}", output.stderr);
    let bytes = output.stdout;
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), SHA);
    let original = bytes.clone();
    let decoded = ntfslog::parse(&bytes);
    let sources: Vec<_> = decoded.records.iter().filter(|r| r.lsn == SOURCE).collect();
    assert_eq!(sources.len(), 1, "independent source identity is required");
    let source = sources[0];
    let identity = Identity {
        lsn: source.lsn,
        offset: usize::try_from(source.offset).unwrap(),
        previous: source.previous_lsn,
        undo: source.undo_next_lsn,
    };
    assert_eq!((identity.previous, identity.undo), (TARGET, TARGET));
    assert!(header_at(&bytes, identity.offset, identity));
    let mut controls = fs::File::create(evidence.join("raw-location-controls.tsv")).unwrap();
    writeln!(controls, "control\tresult").unwrap();
    writeln!(controls, "independent-source-raw-header\taccepted").unwrap();
    for (name, offset) in [
        ("previous-pointer", 8),
        ("undo-pointer", 16),
        ("adjacent-word", 24),
    ] {
        // Even forging the claimed independent offset cannot turn a pointer
        // into the source's full header tuple.
        let forged = Identity {
            offset: identity.offset + offset,
            ..identity
        };
        assert!(
            !header_at(&bytes, forged.offset, forged),
            "{name} accepted as header"
        );
        writeln!(controls, "{name}\trejected").unwrap();
    }
    assert!(!header_at(
        &bytes,
        identity.offset,
        Identity {
            lsn: TARGET,
            ..identity
        }
    ));
    assert!(!header_at(
        &bytes[..identity.offset + 24],
        identity.offset,
        identity
    ));
    writeln!(controls, "wrong-lsn\trejected\ntruncated-header\trejected").unwrap();

    let page_offset = identity.offset / 4096 * 4096;
    let page = &bytes[page_offset..page_offset + 4096];
    let restored = restored_page(page).expect("all eight captured USA trailers must agree");
    for sector in 1..=8 {
        let mut torn = page.to_vec();
        torn[sector * 512 - 2] ^= 1;
        assert!(
            restored_page(&torn).is_err(),
            "torn sector {sector} accepted"
        );
        writeln!(controls, "torn-sector-{sector}\trejected").unwrap();
    }
    for (name, at) in [
        ("wrong-page-magic", 0),
        ("wrong-usa-sequence", 40),
        ("wrong-usa-count", 6),
    ] {
        let mut changed = page.to_vec();
        changed[at] ^= 1;
        assert!(restored_page(&changed).is_err(), "{name} accepted");
        writeln!(controls, "{name}\trejected").unwrap();
    }
    assert!(restored_page(&page[..4095]).is_err());
    writeln!(controls, "truncated-page\trejected").unwrap();
    controls.flush().unwrap();

    // Complete physical source bytes are measured independently of RecordView.
    // The exact length/hash below were measured from completed red artifact
    // 12018493851, not inferred from production acceptance or a replacement log.
    let within = identity.offset - page_offset;
    let length = u32::from_le_bytes(page[within + 24..within + 28].try_into().unwrap()) as usize;
    assert_eq!(length, 88);
    let protected = &page[within..within + 0x30 + length];
    let plain = &restored[within..within + 0x30 + length];
    assert_eq!(
        format!("{:x}", Sha256::digest(protected)),
        "47aadfd93f5b397afd53bd741692ab60814e823267ef681a0da8d925035f28b3"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(plain)),
        "28f8de4d2395365b8fd48426bb07e2bbcdafa01a92849e45eefb669463b51bac"
    );
    let mut physical = fs::File::create(evidence.join("immutable-source-bytes.tsv")).unwrap();
    writeln!(physical, "sha256\toracle_revision\tlsn\tindependent_offset\tclient_data_length\tprotected_sha256\trestored_sha256\tprotected_hex\trestored_hex").unwrap();
    let hex = |data: &[u8]| data.iter().map(|b| format!("{b:02x}")).collect::<String>();
    writeln!(
        physical,
        "{SHA}\t{REV}\t{SOURCE:#x}\t{:#x}\t{length}\t{:x}\t{:x}\t{}\t{}",
        identity.offset,
        Sha256::digest(protected),
        Sha256::digest(plain),
        hex(protected),
        hex(plain)
    )
    .unwrap();
    physical.flush().unwrap();
    fs::write(evidence.join("immutable-source-protected.bin"), protected).unwrap();
    fs::write(evidence.join("immutable-source-restored.bin"), plain).unwrap();
    fs::write(
        evidence.join("immutable-source-restored-page.bin"),
        &restored,
    )
    .unwrap();

    let mut report = fs::File::create(evidence.join("raw-location-roles.tsv")).unwrap();
    writeln!(report, "sha256\toracle_revision\tlsn\traw_offset\tindependent_header_count\tindependent_pointer_owners\theader_attested").unwrap();
    for lsn in [SOURCE, TARGET] {
        for at in (0..=bytes.len() - 0x30)
            .step_by(8)
            .filter(|&at| word(&bytes, at) == Some(lsn))
        {
            let headers = decoded
                .records
                .iter()
                .filter(|r| r.lsn == lsn && r.offset == at as u64)
                .count();
            let mut owners = Vec::new();
            for record in &decoded.records {
                for (field, displacement, value) in [
                    ("previous", 8, record.previous_lsn),
                    ("undo_next", 16, record.undo_next_lsn),
                ] {
                    if value == lsn && record.offset.checked_add(displacement) == Some(at as u64) {
                        owners.push(format!("{:#x}:{field}", record.lsn));
                    }
                }
            }
            let attested = decoded.records.iter().any(|r| {
                r.lsn == lsn
                    && header_at(
                        &bytes,
                        at,
                        Identity {
                            lsn: r.lsn,
                            offset: usize::try_from(r.offset).unwrap(),
                            previous: r.previous_lsn,
                            undo: r.undo_next_lsn,
                        },
                    )
            });
            writeln!(
                report,
                "{SHA}\t{REV}\t{lsn:#x}\t{at:#x}\t{headers}\t{}\t{attested}",
                owners.join(",")
            )
            .unwrap();
            if !owners.is_empty() {
                assert!(
                    !attested,
                    "pointer occurrence was accepted as a record header"
                );
            }
        }
    }
    report.flush().unwrap();
    assert_eq!(
        bytes, original,
        "raw diagnostics changed the Windows capture"
    );
    // Target absence is reported, never asserted to be correct. The separate
    // physical_position test still requires exactly one independently decoded
    // target and fails closed. These controls cannot satisfy that requirement.
}
