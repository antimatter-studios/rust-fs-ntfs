//! CI-only selector controls. Metadata mutations below test selection policy;
//! only the separate fresh-input test supplies genuine independent pair evidence.
use super::*;
use std::{io::Write, path::PathBuf, process::Command, sync::OnceLock};

pub(crate) fn decoded_seed() -> &'static ntfslog::LogFile {
    static SEED: OnceLock<ntfslog::LogFile> = OnceLock::new();
    SEED.get_or_init(|| {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-disks/windows-interrupted-logfile-1.bin.gz");
        let output = Command::new("gzip").arg("-dc").arg(path).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            format!("{:x}", Sha256::digest(&output.stdout)),
            "e6a36b92ace6fa15378af53d9239b1df0aacdf931e4c721d10ce287259d638eb"
        );
        ntfslog::parse(&output.stdout)
    })
}

fn controlled_pair() -> Vec<ntfslog::LogRecord> {
    let mut records: Vec<_> = decoded_seed()
        .records
        .iter()
        .filter(|r| matches!(r.body, ntfslog::Body::Update(_)))
        .take(2)
        .cloned()
        .collect();
    assert_eq!(
        records.len(),
        2,
        "restore the immutable Windows metadata seed"
    );
    assert_ne!(records[0].lsn, records[1].lsn);
    assert_ne!(records[0].offset, records[1].offset);
    records[0].previous_lsn = records[1].lsn;
    records[0].undo_next_lsn = records[1].lsn;
    records[1].previous_lsn = 0;
    records[1].undo_next_lsn = 0;
    records[1].client = records[0].client;
    records[1].transaction_id = records[0].transaction_id;
    records
}

#[test]
fn selector_controls_accept_complete_metadata_and_reject_incomplete_sets() {
    let records = controlled_pair();
    let expected = records[0].lsn;
    assert_eq!(
        complete_pair(&records, MAX_LOG_BYTES, 0).unwrap().lsn,
        expected
    );
    let mut reversed = records.clone();
    reversed.reverse();
    assert_eq!(
        complete_pair(&reversed, MAX_LOG_BYTES, 0).unwrap().lsn,
        expected
    );
    // A pointer occurrence in a decoded source is not a decoded target.
    assert!(complete_pair(&records[..1], MAX_LOG_BYTES, 0).is_err());
    let mut zero_only = records.clone();
    zero_only[0].previous_lsn = 0;
    zero_only[0].undo_next_lsn = 0;
    assert!(complete_pair(&zero_only, MAX_LOG_BYTES, 0).is_err());
    let mut missing_undo = records.clone();
    missing_undo[0].undo_next_lsn = u64::MAX;
    assert!(complete_pair(&missing_undo, MAX_LOG_BYTES, 0).is_err());
    let mut self_link = records.clone();
    self_link[0].previous_lsn = expected;
    assert!(complete_pair(&self_link, MAX_LOG_BYTES, 0).is_err());
    // The physical proof traverses both edges, so one link is not a pair.
    for edge in [0, 1] {
        let mut one_link = records.clone();
        if edge == 0 {
            one_link[0].previous_lsn = 0;
        } else {
            one_link[0].undo_next_lsn = 0;
        }
        assert!(complete_pair(&one_link, MAX_LOG_BYTES, 0).is_err());
    }
    // Only the live log is eligible: at the older of the two LSNs both are
    // live, and one past either of them leaves the source or its target stale.
    let oldest = records[0].lsn.min(records[1].lsn);
    assert_eq!(
        complete_pair(&records, MAX_LOG_BYTES, oldest).unwrap().lsn,
        expected
    );
    for stale in [records[0].lsn + 1, records[1].lsn + 1] {
        assert!(complete_pair(&records, MAX_LOG_BYTES, stale).is_err());
    }
    let checkpoint = decoded_seed()
        .records
        .iter()
        .find(|r| matches!(r.body, ntfslog::Body::Checkpoint(_)))
        .unwrap();
    assert!(complete_pair(std::slice::from_ref(checkpoint), MAX_LOG_BYTES, 0).is_err());
    let mut checkpoint_source = records.clone();
    checkpoint_source[0].body = checkpoint.body.clone();
    assert!(complete_pair(&checkpoint_source, MAX_LOG_BYTES, 0).is_err());
    let mut checkpoint_target = records.clone();
    checkpoint_target[1].body = checkpoint.body.clone();
    assert!(complete_pair(&checkpoint_target, MAX_LOG_BYTES, 0).is_err());
}

#[test]
fn selector_controls_reject_an_actual_independent_pointer_without_its_target() {
    let source = decoded_seed()
        .records
        .iter()
        .find(|r| r.previous_lsn != 0 || r.undo_next_lsn != 0)
        .expect("immutable Windows capture must retain its independent nonzero record");
    assert!(complete_pair(std::slice::from_ref(source), MAX_LOG_BYTES, 0).is_err());
}

#[test]
fn selector_controls_reject_ambiguous_identity_and_wrong_ownership() {
    let records = controlled_pair();
    for duplicate in [0, 1] {
        let mut ambiguous = records.clone();
        ambiguous.push(records[duplicate].clone());
        assert!(complete_pair(&ambiguous, MAX_LOG_BYTES, 0).is_err());
    }
    let mut same_position = records.clone();
    same_position[1].offset = same_position[0].offset;
    assert!(complete_pair(&same_position, MAX_LOG_BYTES, 0).is_err());
    let mut wrong_client = records.clone();
    wrong_client[1].client.sequence ^= 1;
    assert!(complete_pair(&wrong_client, MAX_LOG_BYTES, 0).is_err());
    let mut wrong_transaction = records.clone();
    wrong_transaction[1].transaction_id ^= 1;
    assert!(complete_pair(&wrong_transaction, MAX_LOG_BYTES, 0).is_err());
    let mut outside = records;
    outside[1].offset = u64::MAX;
    assert!(complete_pair(&outside, MAX_LOG_BYTES, 0).is_err());
}

#[test]
fn fresh_pair_report_matches_genuine_independent_records() {
    let raw = env::var("LOGFILE_PAIR_RAW").expect("CI must select a fresh Windows complete pair");
    let expected_sha = env::var("LOGFILE_PAIR_SHA").expect("CI must retain the selected digest");
    let report_path = env::var("LOGFILE_PAIR_REPORT").expect("CI must retain pair metadata");
    let bytes = fs::read(&raw).unwrap();
    assert!(!bytes.is_empty() && bytes.len() as u64 <= MAX_LOG_BYTES);
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected_sha);
    let decoded = ntfslog::parse(&bytes);
    let restart = decoded.restart().unwrap();
    assert_eq!(restart.version.major, 2);
    let report = fs::read_to_string(&report_path).unwrap();
    let rows: Vec<Vec<_>> = report
        .lines()
        .skip(1)
        .map(|r| r.split('\t').collect())
        .collect();
    assert!((2..=3).contains(&rows.len()));
    assert_eq!(rows[0][4], "source");
    let hex = |s: &str| u64::from_str_radix(s.strip_prefix("0x").unwrap(), 16).unwrap();
    let source_lsn = hex(rows[0][5]);
    let sources: Vec<_> = decoded
        .records
        .iter()
        .filter(|r| r.lsn == source_lsn)
        .collect();
    assert_eq!(sources.len(), 1);
    let source = sources[0];
    assert!(source.previous_lsn != 0 || source.undo_next_lsn != 0);
    // Physical proof requires both native traversal edges, beyond the
    // metadata selector's deliberately broader eligibility rule.
    assert_ne!(source.previous_lsn, 0);
    assert_ne!(source.undo_next_lsn, 0);
    let mut physical =
        fs::File::create(PathBuf::from(&report_path).with_file_name("fresh-pair-physical.tsv"))
            .unwrap();
    writeln!(physical, "capture_sha256\toracle_revision\trole\tlsn\tphysical_offset\tclient_data_length\traw_record_sha256\tunprotected_record_sha256\traw_record_hex\tunprotected_record_hex").unwrap();
    let expected_roles: Vec<_> = [
        ("source", source.lsn),
        ("previous_target", source.previous_lsn),
        ("undo_target", source.undo_next_lsn),
    ]
    .into_iter()
    .filter(|(_, lsn)| *lsn != 0)
    .collect();
    assert_eq!(rows.len(), expected_roles.len());
    let mut comparisons = Vec::new();
    for (row, (role, lsn)) in rows.iter().zip(expected_roles) {
        assert_eq!(row.len(), 14);
        assert_eq!(row[0], expected_sha);
        assert_eq!(row[1], ORACLE_REV);
        assert_eq!(row[2], restart.version.major.to_string());
        assert_eq!(row[3], restart.version.minor.to_string());
        assert_eq!(row[4], role);
        assert_eq!(hex(row[5]), lsn);
        let matches: Vec<_> = decoded.records.iter().filter(|r| r.lsn == lsn).collect();
        assert_eq!(
            matches.len(),
            1,
            "a pointer without a unique decoded target is insufficient"
        );
        let record = matches[0];
        assert!(matches!(record.body, ntfslog::Body::Update(_)));
        assert_eq!(record.client, source.client);
        assert_eq!(record.transaction_id, source.transaction_id);
        if role != "source" {
            assert_ne!(record.lsn, source.lsn);
        }
        for (cell, expected) in row[6..13].iter().zip([
            record.offset,
            record.previous_lsn,
            record.undo_next_lsn,
            u64::from(record.client.sequence),
            u64::from(record.client.index),
            u64::from(record.transaction_id),
            u64::from(record.flags),
        ]) {
            assert_eq!(hex(cell), expected);
        }
        assert_eq!(row[13], "update");
        assert_eq!(
            decoded
                .records
                .iter()
                .filter(|r| r.offset == record.offset)
                .count(),
            1
        );
        let at = usize::try_from(record.offset).unwrap();
        let page_size = usize::try_from(restart.log_page_size).unwrap();
        let page_at = at / page_size * page_size;
        let raw_page = &bytes[page_at..page_at + page_size];
        assert_eq!(&raw_page[..4], b"RCRD");
        let u16_at = |data: &[u8], offset: usize| {
            u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap())
        };
        let usa = usize::from(u16_at(raw_page, 4));
        let count = usize::from(u16_at(raw_page, 6));
        assert_eq!(count, page_size / 512 + 1);
        assert!(usa + count * 2 <= page_size);
        let mut page = raw_page.to_vec();
        for sector in 1..count {
            let trailer = sector * 512 - 2;
            assert_eq!(&raw_page[trailer..trailer + 2], &raw_page[usa..usa + 2]);
            page[trailer..trailer + 2]
                .copy_from_slice(&raw_page[usa + sector * 2..usa + sector * 2 + 2]);
        }
        let within = at - page_at;
        let length =
            u32::from_le_bytes(page[within + 24..within + 28].try_into().unwrap()) as usize;
        assert!(
            !record.spans_pages(),
            "native pair requires complete single-page records"
        );
        let end = within.checked_add(48).unwrap().checked_add(length).unwrap();
        assert!(
            end <= page_size,
            "complete native record exceeds its physical page"
        );
        let raw_record = &raw_page[within..end];
        let complete = &page[within..end];
        let copies: Vec<_> = bytes
            .windows(raw_record.len())
            .enumerate()
            .filter_map(|(position, candidate)| (candidate == raw_record).then_some(position))
            .collect();
        assert_eq!(
            copies,
            [at],
            "complete physical record identity is not unique"
        );
        let raw_sha = format!("{:x}", Sha256::digest(raw_record));
        let complete_sha = format!("{:x}", Sha256::digest(complete));
        // This prior full-CI artifact is an immutable, independently selected
        // native pair. New Windows captures also run the same byte controls.
        if expected_sha == "4d8adbd983ab4683d27390c1f1337d1c5906e6c4ca87d6da2dd2b2eb24acb338" {
            assert_eq!(source.lsn, 0x104c22);
            assert_eq!(
                (source.previous_lsn, source.undo_next_lsn),
                (0x104c0d, 0x104c0d)
            );
            let (offset, size, digest) = match record.lsn {
                0x104c22 => (
                    0x16110,
                    392,
                    "994f177d68b660bba58347b0ff09dc23771d2ff3d1506e18a4e31807e70ee85a",
                ),
                0x104c0d => (
                    0x16068,
                    120,
                    "65c31801e07d4a036f2113da9ee0b98ff5098b73aab1073637864e54f841a879",
                ),
                _ => panic!("archived native pair identity changed"),
            };
            assert_eq!(at, offset);
            assert_eq!(length, size);
            assert_eq!(raw_sha, digest);
            assert_eq!(
                complete_sha,
                if record.lsn == 0x104c22 {
                    "c57fa3fef08724e789dbec70569bc900d6dda5023a7d42a2458f00d6abc86f94"
                } else {
                    "65c31801e07d4a036f2113da9ee0b98ff5098b73aab1073637864e54f841a879"
                }
            );
        }
        let hex_bytes = |data: &[u8]| data.iter().map(|b| format!("{b:02x}")).collect::<String>();
        writeln!(physical, "{expected_sha}\t{ORACLE_REV}\t{role}\t{lsn:#x}\t{at:#x}\t{length}\t{raw_sha}\t{complete_sha}\t{}\t{}", hex_bytes(raw_record), hex_bytes(complete)).unwrap();
        physical.flush().unwrap();
        let from_bytes =
            fs_ntfs::logfile_replay::RecordView::parse(&complete[..48], &complete[48..]).unwrap();
        assert_eq!(from_bytes.lsn, record.lsn);
        assert_eq!(from_bytes.client_previous_lsn, record.previous_lsn);
        assert_eq!(from_bytes.client_undo_next_lsn, record.undo_next_lsn);
        assert_eq!(from_bytes.seq_number, record.client.sequence);
        assert_eq!(from_bytes.client_index, record.client.index);
        assert_eq!(from_bytes.transaction_id, record.transaction_id);
        assert_eq!(from_bytes.flags, record.flags);
        assert_eq!(from_bytes.record_type, 1);
        assert_eq!(from_bytes.client_data_length as usize, length);
        let update = record.update().unwrap();
        assert_eq!(from_bytes.redo_operation, update.redo.code());
        assert_eq!(from_bytes.undo_operation, update.undo.code());
        assert_eq!(from_bytes.redo_length, update.redo_length);
        assert_eq!(from_bytes.undo_length, update.undo_length);
        for (offset, expected) in [
            (from_bytes.redo_offset, &update.redo_data),
            (from_bytes.undo_offset, &update.undo_data),
        ] {
            let start = 48 + usize::from(offset);
            assert!(start + expected.len() <= complete.len());
            assert_eq!(&complete[start..start + expected.len()], expected);
        }
        comparisons.push((role, lsn, from_bytes));
        let wrong = fs_ntfs::logfile_replay::RecordView::parse(
            &page[within + 8..within + 8 + 48],
            &page[within + 8 + 48..],
        )
        .unwrap();
        assert_ne!(
            wrong.lsn, lsn,
            "adjacent physical position accepted as this record"
        );
    }
    // Retain every complete physical record before grading production, so a
    // source refusal cannot hide its target's independent bytes or result.
    assert_eq!(comparisons.len(), 3);
    let mut outcomes =
        fs::File::create(PathBuf::from(&report_path).with_file_name("fresh-pair-strict.tsv"))
            .unwrap();
    writeln!(
        outcomes,
        "role\tlsn\texpected\tstrict_result\tadjacent_lsn_result"
    )
    .unwrap();
    let mut failures = Vec::new();
    for (role, lsn, expected) in comparisons {
        let actual = record_at_lsn(&bytes, lsn, bytes.len());
        let adjacent = record_at_lsn(&bytes, lsn + 1, bytes.len());
        writeln!(
            outcomes,
            "{role}\t{lsn:#x}\t{expected:?}\t{actual:?}\t{adjacent:?}"
        )
        .unwrap();
        outcomes.flush().unwrap();
        match actual {
            Ok(view) if view == expected => {}
            result => failures.push(format!(
                "{role} {lsn:#x}: strict lookup disagrees: {result:?}"
            )),
        }
        if adjacent.is_ok() {
            failures.push(format!("{role} {lsn:#x}: adjacent LSN position accepted"));
        }
    }
    assert_eq!(fs::read(raw).unwrap(), bytes, "fresh pair source changed");
    assert!(failures.is_empty(), "{}", failures.join("; "));
}
