//! One field of one Windows-authored list entry is changed per copy (#821).
//! CI mounts these exact copies and requires Windows chkdsk to find damage.
mod common;

use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::{BlockIo, PathIo};
use fs_ntfs::fsck::{self, CheckFinding};
use fs_ntfs::{data_runs, mft_io, read, write};
use std::path::{Path, PathBuf};

fn image(tag: &str) -> PathBuf {
    match std::env::var_os("NTFS_LIST_CHECK_IMAGES") {
        Some(dir) => {
            std::fs::create_dir_all(&dir).unwrap();
            PathBuf::from(dir).join(format!("{tag}.img"))
        }
        None => common::temp_image_path(format!("list_attribute_{tag}")).into(),
    }
}

fn copy(tag: &str) -> (PathBuf, u64, Vec<u8>) {
    let fixture = Path::new("test-disks/ntfs-attrlist.img");
    assert!(
        fixture.exists(),
        "run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let img = image(tag);
    std::fs::copy(fixture, &img).unwrap();
    let mut io = PathIo::open_ro(&img).unwrap();
    let base = write::resolve_path_to_record_number_io(&mut io, "/many.bin").unwrap();
    let list = read::read_attribute_value(&mut io, base, AttrType::AttributeList, None).unwrap();
    (img, base, list)
}

fn write_list(io: &mut PathIo, base: u64, value: &[u8]) {
    let (params, record) = mft_io::read_mft_record_io(io, base).unwrap();
    let loc = attr_io::find_attribute(&record, AttrType::AttributeList, None).unwrap();
    if let Some(offset) = loc.resident_value_offset {
        mft_io::update_mft_record_io(io, base, |record| {
            let at = loc.attr_offset + offset as usize;
            record[at..at + value.len()].copy_from_slice(value);
            Ok(())
        })
        .unwrap();
    } else {
        let at = loc.attr_offset + loc.non_resident_mapping_pairs_offset.unwrap() as usize;
        let runs = data_runs::decode_runs(&record[at..loc.attr_offset + loc.attr_length]).unwrap();
        let mut done = 0;
        for run in runs {
            let count = ((run.length * params.cluster_size) as usize).min(value.len() - done);
            <PathIo as BlockIo>::write_all_at(
                io,
                run.lcn.unwrap() * params.cluster_size,
                &value[done..done + count],
            )
            .unwrap();
            done += count;
            if done == value.len() {
                break;
            }
        }
        assert_eq!(done, value.len());
    }
    <PathIo as BlockIo>::sync(io).unwrap();
}

#[test]
fn the_untouched_windows_attribute_list_is_clean() {
    let (img, _, _) = copy("clean");
    let mut io = PathIo::open_ro(&img).unwrap();
    assert!(fsck::check_io(&mut io).unwrap().findings.is_empty());
}

#[test]
fn each_entry_field_must_identify_an_attribute_in_its_record() {
    let mut missed = Vec::new();
    for tag in ["type", "name", "vcn", "instance"] {
        let (img, base, mut list) = copy(tag);
        let mut at = 0;
        loop {
            let entry = read::parse_attribute_list(&list[at..]).unwrap().remove(0);
            if entry.record_number != base
                && entry.type_code == AttrType::Data as u32
                && entry.name.is_some()
            {
                break;
            }
            at += u16::from_le_bytes([list[at + 4], list[at + 5]]) as usize;
        }
        match tag {
            "type" => list[at..at + 4].copy_from_slice(&(AttrType::Bitmap as u32).to_le_bytes()),
            "name" => {
                let name = at + list[at + 7] as usize;
                list[name] ^= 1;
            }
            "vcn" => list[at + 8..at + 16].copy_from_slice(&1u64.to_le_bytes()),
            "instance" => list[at + 24..at + 26].copy_from_slice(&u16::MAX.to_le_bytes()),
            _ => unreachable!(),
        }
        let mut io = PathIo::open_rw(&img).unwrap();
        write_list(&mut io, base, &list);
        let report = fsck::check_io(&mut io).unwrap();
        if std::env::var_os("NTFS_LIST_CHECK_IMAGES").is_some() {
            std::fs::write(img.with_extension("findings.txt"), format!("{report:#?}")).unwrap();
        }
        if !report.findings.iter().any(|f| {
            matches!(f,
                CheckFinding::BadAttributeList { record, reason }
                    if *record == base && reason.contains(&format!("entry at {at}"))
                        && reason.contains("instance")
            )
        }) {
            missed.push(tag);
        }
    }
    assert!(
        missed.is_empty(),
        "entry mismatches reported clean: {missed:?}"
    );
}

fn remove_extension_entries(whole_record: bool) {
    let tag = if whole_record {
        "unlisted-record"
    } else {
        "unlisted-attribute"
    };
    let (img, base, list) = copy(tag);
    let entries = read::parse_attribute_list(&list).unwrap();
    let entry = entries
        .iter()
        .find(|e| {
            e.record_number != base
                && e.name.is_some()
                && entries
                    .iter()
                    .filter(|p| p.record_number == e.record_number)
                    .count()
                    > 1
        })
        .expect("a Windows extension holds multiple attributes");
    let mut kept = Vec::new();
    let mut at = 0;
    for e in &entries {
        let len = u16::from_le_bytes([list[at + 4], list[at + 5]]) as usize;
        if e.record_number != entry.record_number
            || (!whole_record && e.attribute_id != entry.attribute_id)
        {
            kept.extend_from_slice(&list[at..at + len]);
        }
        at += len;
    }
    let mut io = PathIo::open_rw(&img).unwrap();
    write_list(&mut io, base, &kept);
    // Only the list loses entries. Its allocation, and all extension records,
    // stay untouched; shorten the value so stale tail bytes are not entries.
    mft_io::update_mft_record_io(&mut io, base, |record| {
        let loc = attr_io::find_attribute(record, AttrType::AttributeList, None).unwrap();
        if loc.is_resident {
            record[loc.attr_offset + 16..loc.attr_offset + 20]
                .copy_from_slice(&(kept.len() as u32).to_le_bytes());
        } else {
            for offset in [48, 56] {
                record[loc.attr_offset + offset..loc.attr_offset + offset + 8]
                    .copy_from_slice(&(kept.len() as u64).to_le_bytes());
            }
        }
        Ok(())
    })
    .unwrap();
    <PathIo as BlockIo>::sync(&mut io).unwrap();
    drop(io);
    let before = std::fs::read(&img).unwrap();
    let mut io = PathIo::open_ro(&img).unwrap();
    let report = fsck::check_io(&mut io).unwrap();
    if std::env::var_os("NTFS_LIST_CHECK_IMAGES").is_some() {
        std::fs::write(img.with_extension("findings.txt"), format!("{report:#?}")).unwrap();
    }
    assert!(
        report.findings.iter().any(|f| matches!(f,
            CheckFinding::BadAttributeList { record, reason }
                if *record == base
                    && reason.contains(&format!("extension record {}", entry.record_number))
                    && reason.contains("no $ATTRIBUTE_LIST entry")
                    && reason.contains(&format!("type {:#x}", entry.type_code))
                    && reason.contains(&format!("name {:?}", entry.name.as_deref().unwrap()))
                    && reason.contains(&format!("lowest VCN {}", entry.starting_vcn))
                    && reason.contains(&format!("instance {}", entry.attribute_id))
        )),
        "{report:#?}"
    );
    assert_eq!(
        std::fs::read(&img).unwrap(),
        before,
        "checking must not write"
    );
}

#[test]
fn an_extension_attribute_with_no_list_entry_is_reported() {
    remove_extension_entries(false);
}

#[test]
fn an_extension_record_with_no_list_entries_is_reported() {
    remove_extension_entries(true);
}
