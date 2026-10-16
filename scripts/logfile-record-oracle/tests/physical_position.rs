//! CI-only physical evidence; expected positions never come from RecordView.
use std::{fs, io::Write, path::PathBuf, process::Command};

use fs_ntfs::logfile_replay::record_at_lsn;
use sha2::{Digest, Sha256};

const SHA: &str = "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb";
const REV: &str = "b88585646801ee2c1e2acf91a078da6ab57af778";
/// The live log's first independently decoded record with a nonzero link
/// whose targets are live too, and its previous/undo-next target. Live means
/// no older than the NTFS client's oldest LSN (`0x33e786`); the pass-2 pair
/// `0x1046bb`/`0x10443c` this test first named survives only in a stale tail
/// copy, and `0x10443c` was overwritten by later passes.
const SOURCE: u64 = 0x33e7c6;
const TARGET: u64 = 0x33e7a5;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn word(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[test]
fn windows_nonzero_record_and_link_have_independent_physical_evidence() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let evidence = root.join("tmp/logfile-physical-position");
    fs::create_dir_all(&evidence).expect("create physical evidence directory");
    let output = Command::new("gzip")
        .arg("-dc")
        .arg(root.join("test-disks/windows-interrupted-logfile-1.bin.gz"))
        .output()
        .expect("install gzip to read the immutable Windows capture");
    assert!(output.status.success(), "gzip: {:?}", output.stderr);
    let bytes = output.stdout;
    assert!(!bytes.is_empty() && bytes.len() <= 64 * 1024 * 1024);
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), SHA);
    let original = bytes.clone();
    let decoded = ntfslog::parse(&bytes);
    fs::write(evidence.join("independent.txt"), format!("{decoded:#?}\n"))
        .expect("retain independent records, offsets and decoder problems");
    let restart = decoded.restart().expect("independent Windows restart page");
    assert_eq!(restart.version.major, 2);
    let bits = restart.area.sequence_number_bits;
    assert!((3..64).contains(&bits));
    let page_size = usize::try_from(restart.log_page_size).unwrap();
    assert!(page_size.is_power_of_two() && page_size >= 512);
    // The independent RestartArea documents the low bits as file offset / 8.
    // This calculation does not call or duplicate production's page selector.
    let address_mask = u64::MAX >> bits;
    let mut report = fs::File::create(evidence.join("positions.tsv")).unwrap();
    writeln!(report, "sha256\toracle_revision\tlsn\tprevious_lsn\tundo_next_lsn\toracle_physical_offset\tlsn_byte_address\tpage_size\tsequence_bits\traw_physical_lsn\traw_addressed_lsn\traw_header_hex").unwrap();
    let mut hits = fs::File::create(evidence.join("raw-lsn-hits.tsv")).unwrap();
    writeln!(
        hits,
        "lsn\traw_byte_offset\tpage_offset\twithin_page\tpage_sha256\tpage_header_hex\theader_hex"
    )
    .unwrap();
    let mut outcomes = fs::File::create(evidence.join("strict-lookup.txt")).unwrap();
    let mut copy_report = fs::File::create(evidence.join("raw-copies.tsv")).unwrap();
    writeln!(
        copy_report,
        "lsn\tindependent_offset\tcopy_count\traw_header_copies"
    )
    .unwrap();
    let mut failures = Vec::new();
    // Both targets must be in the live log, judged by the independent
    // restart area's NTFS client, never by production's reader.
    let oldest = restart
        .area
        .clients
        .iter()
        .find(|client| client.name == "NTFS")
        .expect("independent NTFS client in the Windows restart area")
        .oldest_lsn;
    for lsn in [SOURCE, TARGET] {
        if lsn < oldest {
            failures.push(format!(
                "{lsn:#x} is older than the client's oldest LSN {oldest:#x}: not in the live log"
            ));
        }
    }
    let mut raw_hit_counts = Vec::new();
    for lsn in [SOURCE, TARGET] {
        // Raw evidence is collected for every target before decoder identity
        // is graded, so one missing or ambiguous record cannot hide the other
        // target's aligned hits, pages or addressed page.
        let addressed = usize::try_from((lsn & address_mask).checked_mul(8).unwrap()).unwrap();
        assert!(addressed + 8 <= bytes.len());
        let home = addressed / page_size * page_size;
        fs::write(
            evidence.join(format!("lsn-{lsn:x}-addressed-page.bin")),
            &bytes[home..(home + page_size).min(bytes.len())],
        )
        .unwrap();
        let mut raw_hits = 0usize;
        // Keep every aligned raw occurrence, including possible payload hits;
        // only the independent decoder identifies one as a record header.
        for at in (0..=bytes.len() - 0x30).step_by(8) {
            if bytes[at..at + 8] == lsn.to_le_bytes() {
                let page = at / page_size * page_size;
                let raw_page = &bytes[page..(page + page_size).min(bytes.len())];
                writeln!(
                    hits,
                    "{lsn:#x}\t{at:#x}\t{page:#x}\t{:#x}\t{:x}\t{}\t{}",
                    at % page_size,
                    Sha256::digest(raw_page),
                    hex(&raw_page[..0x40]),
                    hex(&bytes[at..at + 0x30])
                )
                .unwrap();
                fs::write(
                    evidence.join(format!("lsn-{lsn:x}-page-{page:x}.bin")),
                    raw_page,
                )
                .unwrap();
                raw_hits += 1;
            }
        }
        hits.flush().unwrap();
        raw_hit_counts.push((lsn, raw_hits));
        // The existing API's result is recorded as evidence beside the raw
        // hits; it is never used as the identity oracle.
        match record_at_lsn(&bytes, lsn, bytes.len()) {
            Ok(view) => writeln!(outcomes, "{lsn:#x}\taccepted\t{view:#?}").unwrap(),
            Err(error) => writeln!(outcomes, "{lsn:#x}\trefused\t{error}").unwrap(),
        }
        outcomes.flush().unwrap();
        let matches: Vec<_> = decoded.records.iter().filter(|r| r.lsn == lsn).collect();
        if matches.len() != 1 {
            // Exactly one independent record is still required; the shortfall
            // is recorded and fails the test after the other target is observed.
            writeln!(
                report,
                "{SHA}\t{REV}\t{lsn:#x}\tindependent_match_count={}\trequired=1\t-\t{addressed:#x}\t{page_size:#x}\t{bits}\t-\t{:#x}\t-",
                matches.len(),
                word(&bytes, addressed)
            )
            .unwrap();
            report.flush().unwrap();
            writeln!(
                copy_report,
                "{lsn:#x}\tindependent_match_count={}\t-\t-",
                matches.len()
            )
            .unwrap();
            copy_report.flush().unwrap();
            failures.push(format!(
                "independent identity ambiguous/missing: {lsn:#x} matched {} records, required 1; see independent.txt",
                matches.len()
            ));
            continue;
        }
        let record = matches[0];
        let physical = usize::try_from(record.offset).unwrap();
        assert!(physical + 0x30 <= bytes.len());
        writeln!(report, "{SHA}\t{REV}\t{lsn:#x}\t{:#x}\t{:#x}\t{physical:#x}\t{addressed:#x}\t{page_size:#x}\t{bits}\t{:#x}\t{:#x}\t{}", record.previous_lsn, record.undo_next_lsn, word(&bytes, physical), word(&bytes, addressed), hex(&bytes[physical..physical + 0x30])).unwrap();
        report.flush().unwrap();
        // Copy identity: a raw position is a header copy of this record only
        // when its raw lsn/previous/undo words equal the independent decode.
        // The selected copy must be exactly the decoder's own file offset.
        let is_copy = |at: usize| {
            at + 0x30 <= bytes.len()
                && [
                    word(&bytes, at),
                    word(&bytes, at + 8),
                    word(&bytes, at + 16),
                ] == [lsn, record.previous_lsn, record.undo_next_lsn]
        };
        let copies: Vec<usize> = (0..=bytes.len() - 0x30)
            .step_by(8)
            .filter(|&at| is_copy(at))
            .collect();
        writeln!(
            copy_report,
            "{lsn:#x}\t{physical:#x}\t{}\t{copies:x?}",
            copies.len()
        )
        .unwrap();
        copy_report.flush().unwrap();
        if copies != [physical] {
            failures.push(format!(
                "{lsn:#x}: raw header copies {copies:x?} are not exactly independent offset {physical:#x}"
            ));
        }
        // Rejection control: a wrong position must not satisfy the identity.
        assert!(
            !is_copy(physical + 8),
            "{lsn:#x}: wrong-position control accepted"
        );
        // A strict-lookup refusal above is evidence, not a pass for the
        // unchanged positive nonzero-link integration assertion.
        // This fixture-specific byte control deliberately fails if a USA
        // trailer intervenes; raw bytes must not be silently unprotected or
        // replaced with production-decoded values to make evidence agree.
        if physical % 512 + 24 > 510 {
            failures.push(format!(
                "{lsn:#x}: raw identity/link tuple crosses a sector trailer"
            ));
        } else if [
            word(&bytes, physical),
            word(&bytes, physical + 8),
            word(&bytes, physical + 16),
        ] != [lsn, record.previous_lsn, record.undo_next_lsn]
        {
            failures.push(format!(
                "{lsn:#x}: raw physical tuple disagrees with independent record"
            ));
        }
        if lsn == SOURCE && (record.previous_lsn != TARGET || record.undo_next_lsn != TARGET) {
            failures.push("immutable independent nonzero links changed".into());
        }
    }
    hits.flush().unwrap();
    // Both targets were scanned whatever the identity outcome; the per-target
    // raw hit counts are evidence that neither target's observation was cut short.
    fs::write(
        evidence.join("raw-lsn-hit-counts.tsv"),
        raw_hit_counts
            .iter()
            .map(|(lsn, n)| format!("{lsn:#x}\t{n}\n"))
            .collect::<String>(),
    )
    .unwrap();
    assert_eq!(raw_hit_counts.len(), 2, "a target LSN was not scanned");
    assert_eq!(bytes, original, "position experiment changed Windows bytes");
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
