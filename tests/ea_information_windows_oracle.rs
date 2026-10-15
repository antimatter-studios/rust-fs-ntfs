//! The eight-byte EA summary is compared with bytes Windows wrote.
//!
//! The fixture builder calls NtSetEaFile with the same two entries and zero,
//! one or two FILE_NEED_EA flags. No image is checked into the repository.

mod common;

use fs_ntfs::ea_io::{self, Ea};
use ntfs::{NtfsAttributeType, NtfsReadSeek};
use std::path::Path;

const IMAGE: &str = "test-disks/ntfs-ea.img";

fn windows_summary(path: &str) -> [u8; 8] {
    assert!(
        Path::new(IMAGE).is_file(),
        "missing {IMAGE}; run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let (ntfs, mut reader) = common::open(IMAGE);
    let file = common::navigate(&ntfs, &mut reader, path);
    let mut attributes = file.attributes();
    while let Some(item) = attributes.next(&mut reader) {
        let item = item.expect("attribute item");
        let attribute = item.to_attribute().expect("attribute");
        if attribute.ty().expect("attribute type") != NtfsAttributeType::EAInformation {
            continue;
        }
        assert_eq!(attribute.value_length(), 8, "Windows summary size");
        let mut value = attribute.value(&mut reader).expect("Windows summary value");
        let mut bytes = [0u8; 8];
        value
            .read_exact(&mut reader, &mut bytes)
            .expect("read Windows summary");
        return bytes;
    }
    panic!("Windows file {path} has no $EA_INFORMATION");
}

#[test]
fn the_ea_summary_layout_and_lengths_match_windows() {
    for need_count in 0..=2u16 {
        let entries = vec![
            Ea {
                name: b"A".to_vec(),
                value: b"aaa".to_vec(),
                flags: if need_count == 2 {
                    ea_io::FLAG_NEED_EA
                } else {
                    0
                },
            },
            Ea {
                name: b"LONG".to_vec(),
                value: b"x".to_vec(),
                flags: if need_count > 0 {
                    ea_io::FLAG_NEED_EA
                } else {
                    0
                },
            },
        ];
        // Literal lengths come from the two input entries, separately from
        // the implementation under test: compact 9 + 10, aligned 16 + 16.
        let windows = windows_summary(&format!("/ea-summary-{need_count}.bin"));
        assert_eq!(windows, [19, 0, need_count as u8, 0, 32, 0, 0, 0]);
        assert_eq!(
            ea_io::packed_ea_length(&entries).expect("compact length"),
            19
        );
        assert_eq!(ea_io::encode(&entries).expect("query entries").len(), 32);
        assert_eq!(ea_io::count_need_ea(&entries), u32::from(need_count));
        assert_eq!(
            ea_io::build_ea_information_value(19, need_count, 32),
            windows
        );
    }
}
