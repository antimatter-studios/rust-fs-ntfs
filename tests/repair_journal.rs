#![cfg(feature = "plan-file")]

use fs_ntfs::repair_journal::{
    applied_record, header_record, intent_record, read_journal, JournalBinding, JournalFault,
    JournalRecord, TornRecord,
};
use fs_ntfs::repair_plan::{PlanUnit, PlannedWrite};

fn binding() -> JournalBinding {
    JournalBinding {
        plan_sha256: [0x35; 32],
        source_size: 4096,
        volume_serial: 17,
    }
}

fn records() -> Vec<Vec<u8>> {
    let unit = PlanUnit {
        writes: vec![PlannedWrite {
            offset: 512,
            before: vec![0x12; 16],
            after: vec![0x34; 16],
        }],
    };
    vec![
        header_record(&binding()),
        intent_record(0, &unit),
        applied_record(0),
    ]
}

#[test]
fn every_byte_cut_returns_only_complete_records() {
    let encoded = records();
    let bytes = encoded.concat();
    let whole = read_journal(&bytes, &binding()).unwrap();
    for cut in 0..=bytes.len() {
        let mut offset = 0;
        let mut count = 0;
        for record in &encoded {
            if offset + record.len() > cut {
                break;
            }
            offset += record.len();
            count += 1;
        }
        let back = read_journal(&bytes[..cut], &binding()).unwrap();
        assert_eq!(back.records, whole.records[..count], "cut at {cut}");
        assert_eq!(
            back.torn,
            (offset != cut).then_some(TornRecord {
                record: count,
                offset: offset as u64,
            }),
            "cut at {cut}"
        );
    }
}

#[test]
fn every_single_byte_flip_in_a_complete_journal_is_refused() {
    let encoded = records();
    let bytes = encoded.concat();
    let mut start = 0;
    for (index, record) in encoded.iter().enumerate() {
        for position in start..start + record.len() {
            let mut damaged = bytes.clone();
            damaged[position] ^= 0x80;
            let error = read_journal(&damaged, &binding())
                .expect_err("a changed complete record must be refused");
            assert_eq!(error.record, index, "byte {position}");
            assert_eq!(error.offset, start as u64, "byte {position}");
            assert_eq!(
                error.fault,
                JournalFault::ChecksumMismatch,
                "byte {position}"
            );
        }
        start += record.len();
    }
}

#[test]
fn a_length_flip_in_a_middle_record_is_not_a_torn_tail() {
    let encoded = records();
    let mut bytes = encoded.concat();
    let start = encoded[0].len();
    bytes[start + 5] ^= 0x80;
    let error = read_journal(&bytes, &binding())
        .expect_err("a changed middle length must not hide the complete applied record");
    assert_eq!(error.record, 1);
    assert_eq!(error.offset, start as u64);
    assert_eq!(error.fault, JournalFault::ChecksumMismatch);
}

#[test]
fn an_empty_file_has_neither_records_nor_a_torn_tail() {
    let journal = read_journal(&[], &binding()).unwrap();
    assert!(journal.records.is_empty());
    assert_eq!(journal.torn, None);
}

#[test]
fn a_torn_tail_must_be_removed_before_reappending_the_record() {
    let encoded = records();
    let bytes = encoded.concat();
    let start = encoded[0].len();
    for cut in start + 1..start + encoded[1].len() {
        let mut resumed = bytes[..cut].to_vec();
        let torn = read_journal(&resumed, &binding()).unwrap().torn.unwrap();
        assert_eq!(torn.offset, start as u64);
        resumed.truncate(torn.offset as usize);
        resumed.extend_from_slice(&bytes[start..]);
        assert_eq!(
            read_journal(&resumed, &binding()).unwrap(),
            read_journal(&bytes, &binding()).unwrap(),
            "cut at {cut}"
        );
    }
}

#[test]
fn appending_after_a_torn_tail_does_not_make_a_valid_journal() {
    let encoded = records();
    let mut bytes = encoded[0].clone();
    bytes.extend_from_slice(&encoded[1][..9]);
    bytes.extend_from_slice(&encoded[1]);
    bytes.extend_from_slice(&encoded[2]);
    assert!(read_journal(&bytes, &binding()).is_err());
}

#[test]
fn a_complete_intent_without_an_applied_record_stays_open() {
    let encoded = records();
    let bytes = encoded[..2].concat();
    let journal = read_journal(&bytes, &binding()).unwrap();
    assert_eq!(journal.torn, None);
    assert!(matches!(
        journal.records.last(),
        Some(JournalRecord::Intent { unit: 0, .. })
    ));
}

fn bytes_from_hex(encoded: &str) -> Vec<u8> {
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn version_two_bytes_match_independent_sha256_vectors() {
    let expected = bytes_from_hex(concat!(
        "010000003c000000c48c5fcac2be2d84b9de1050d49d365d2f1539260af438ddef7c90e0895cde8a4e544653",
        "4a524e4c02000000353535353535353535353535353535353535353535353535353535353535353500100000",
        "0000000011000000000000003e9bb6dc7695a6e27cb3c83db6371de1c96808ec07a3f5458d0ebec8bfc19dab",
        "0200000034000000243ba0b5d0746ae5ba7332c71d492c44c07e6cf66f22b7e3fa02ab1672ff6d4900000000",
        "0100000000020000000000001000000012121212121212121212121212121212343434343434343434343434",
        "3434343457cf024d958dc3b04454a237a9464466dad9172ad5b560c207f4d12673978c730300000004000000",
        "8073c94ef47ecc86dcd78a8d9027a23484fadcd7cea37150319ba8cbf1c70b6b0000000009711b75eebcaf89",
        "322ba8e8478a9605838110ae08763ec5ded9d1c20e9c08dc",
    ));
    let encoded = records().concat();
    assert_eq!(encoded, expected);
    let journal = read_journal(&expected, &binding()).unwrap();
    assert_eq!(journal.records.len(), 3);
    assert_eq!(journal.torn, None);
}

#[test]
fn version_one_framing_is_refused() {
    let bytes = bytes_from_hex(concat!(
        "010000003c0000004e5446534a524e4c01000000353535353535353535353535353535353535353535353535",
        "353535353535353500100000000000001100000000000000f5859315c2909210118b27e9d967cba7dddc9fdf",
        "b2214d4f1d0e2b68b30d0b24",
    ));
    let error = read_journal(&bytes, &binding()).unwrap_err();
    assert_eq!(error.record, 0);
    assert_eq!(error.offset, 0);
    assert_eq!(error.fault, JournalFault::ChecksumMismatch);
}
