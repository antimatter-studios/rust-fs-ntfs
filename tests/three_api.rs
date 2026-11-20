use fs_core::{BlockDevice, BlockRead};
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::file::NtfsFileAPI;
use fs_ntfs::primitives::{inspect_file_record, AttributeValue, NtfsPrimitiveAPI};
use fs_ntfs::volume::{NtfsVolumeAPI, RawVolume, Volume};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl BlockRead for Memory {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        let data = self.0.lock().unwrap();
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(output.len()).map(|end| (start, end)))
            .and_then(|(start, end)| data.get(start..end));
        output.copy_from_slice(range.ok_or(fs_core::Error::OutOfBounds {
            offset,
            len: output.len() as u64,
            size: data.len() as u64,
        })?);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.lock().unwrap().len() as u64
    }
}

impl BlockDevice for Memory {
    fn write_at(&self, offset: u64, input: &[u8]) -> fs_core::Result<()> {
        let mut data = self.0.lock().unwrap();
        let size = data.len() as u64;
        let range = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(input.len()).map(|end| (start, end)))
            .and_then(|(start, end)| data.get_mut(start..end));
        range
            .ok_or(fs_core::Error::OutOfBounds {
                offset,
                len: input.len() as u64,
                size,
            })?
            .copy_from_slice(input);
        Ok(())
    }
    fn is_writable(&self) -> bool {
        true
    }
}

impl BlockIo for Memory {
    fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), String> {
        self.read_at(offset, output)
            .map_err(|error| error.to_string())
    }
    fn write_all_at(&mut self, offset: u64, input: &[u8]) -> Result<(), String> {
        self.write_at(offset, input)
            .map_err(|error| error.to_string())
    }
    fn size(&self) -> u64 {
        self.size_bytes()
    }
}

fn formatted() -> Memory {
    let size = 8 * 1024 * 1024;
    let mut memory = Memory(Arc::new(Mutex::new(vec![0; size])));
    fs_ntfs::mkfs::format_filesystem(
        &mut memory,
        size as u64,
        4096,
        1024,
        Some("THREE API"),
        Some(0x123456789abcdef0),
    )
    .unwrap();
    memory
}

fn populated(payload: &[u8]) -> (Memory, u64) {
    let device = formatted();
    let volume = Volume::open_writable(device.clone()).unwrap();
    let number = volume.create_file("/", "treasure.txt").unwrap();
    volume.write_file_at("/treasure.txt", 0, payload).unwrap();
    volume.flush().unwrap();
    (device, number)
}

fn data_attribute(record: &fs_ntfs::primitives::RecordEvidence) -> usize {
    record
        .attributes
        .iter()
        .position(|attribute| {
            attribute.type_code == AttrType::Data as u32 && attribute.name.is_empty()
        })
        .unwrap()
}

#[test]
fn file_api_reads_what_the_independent_ntfs_reader_sees() {
    use ntfs::NtfsReadSeek;
    let payload = vec![0x5a; 12000];
    let (device, number) = populated(&payload);
    let volume = Volume::open(device.clone()).unwrap();
    assert_eq!(
        volume.volume_info().unwrap().serial_number,
        0x123456789abcdef0
    );
    assert!(volume
        .read_directory("/")
        .unwrap()
        .iter()
        .any(|entry| entry.name == "treasure.txt" && entry.record_number == number));
    assert_eq!(
        volume.metadata("/treasure.txt").unwrap().size,
        payload.len() as u64
    );
    let file = volume.open_file("/treasure.txt").unwrap();
    assert_eq!(file.record_number(), number);
    let mut output = vec![0; payload.len()];
    assert_eq!(
        volume.read_file_at(&file, 0, &mut output).unwrap(),
        payload.len()
    );
    assert_eq!(output, payload);
    assert_eq!(
        volume
            .read_file_at(&file, payload.len() as u64, &mut output)
            .unwrap(),
        0
    );
    assert_eq!(
        volume
            .read_stream_at("/treasure.txt", "", 4090, 30)
            .unwrap(),
        payload[4090..4120]
    );
    let bytes = device.0.lock().unwrap().clone();
    let mut cursor = std::io::Cursor::new(bytes);
    let independent = ntfs::Ntfs::new(&mut cursor).unwrap();
    let record = independent.file(&mut cursor, number).unwrap();
    let item = record.data(&mut cursor, "").unwrap().unwrap();
    let attribute = item.to_attribute().unwrap();
    let mut value = attribute.value(&mut cursor).unwrap();
    let mut expected = vec![0; payload.len()];
    assert_eq!(
        value.read(&mut cursor, &mut expected).unwrap(),
        payload.len()
    );
    assert_eq!(expected, output);
}

#[test]
fn writable_file_api_reuses_the_existing_mutators() {
    let device = formatted();
    let volume = Volume::open_writable(device.clone()).unwrap();
    volume.create_directory("/", "hold").unwrap();
    volume.create_file("/hold", "before.txt").unwrap();
    volume
        .write_file_at("/hold/before.txt", 0, b"hello world")
        .unwrap();
    volume.set_len("/hold/before.txt", 5).unwrap();
    volume.rename("/hold/before.txt", "after.txt").unwrap();
    assert_eq!(
        volume
            .read_stream_at("/hold/after.txt", "", 0, 100)
            .unwrap(),
        b"hello"
    );
    volume.remove_file("/hold/after.txt").unwrap();
    volume.remove_directory("/hold").unwrap();
    assert!(volume.metadata("/hold").is_err());
    volume.flush().unwrap();
}

#[test]
fn read_only_mount_never_forwards_any_mutation() {
    let (device, _) = populated(b"kept");
    let before = device.0.lock().unwrap().clone();
    let volume = Volume::open(device.clone()).unwrap();
    assert!(!volume.is_writable());
    assert_eq!(volume.geometry().unwrap().serial_number, 0x123456789abcdef0);
    assert!(volume.create_file("/", "no").is_err());
    assert!(volume.create_directory("/", "no").is_err());
    assert!(volume.write_file_at("/treasure.txt", 0, b"no").is_err());
    assert!(volume.set_len("/treasure.txt", 0).is_err());
    assert!(volume.rename("/treasure.txt", "no").is_err());
    assert!(volume.remove_file("/treasure.txt").is_err());
    assert!(volume.remove_directory("/").is_err());
    assert!(volume.write_volume_bytes(0, b"no").is_err());
    volume.flush().unwrap();
    assert_eq!(*device.0.lock().unwrap(), before);
}

#[test]
fn handles_cannot_cross_volumes_or_outlive_their_record() {
    let (device, _) = populated(b"kept");
    let volume = Volume::open_writable(device.clone()).unwrap();
    let file = volume.open_file("/treasure.txt").unwrap();
    let other = Volume::open(device).unwrap();
    assert!(other.read_file_at(&file, 0, &mut [0; 4]).is_err());
    assert!(volume.open_file("/").is_err());
    volume.remove_file("/treasure.txt").unwrap();
    assert!(volume.read_file_at(&file, 0, &mut [0; 4]).is_err());
}

#[test]
fn primitive_api_recovers_a_deleted_resident_file_without_the_namespace() {
    let (device, number) = populated(b"buried treasure");
    let volume = Volume::open_for_salvage(device.clone()).unwrap();
    let original = volume.inspect_mft_record(number).unwrap();
    let mut fixed = original.fixed.unwrap();
    fixed[22..24].copy_from_slice(&0u16.to_le_bytes());
    fs_ntfs::mft_io::apply_fixup_on_write(&mut fixed, 512).unwrap();
    device.write_at(original.offset, &fixed).unwrap();
    let before = device.0.lock().unwrap().clone();
    let record = volume.inspect_mft_record(number).unwrap();
    assert!(!record.in_use);
    assert!(record.issues.is_empty());
    assert!(volume.open_file("/treasure.txt").is_err());
    let mut output = [0; 32];
    let count = volume
        .read_salvage_stream_at(&record, data_attribute(&record), 4096, 0, &mut output)
        .unwrap();
    assert_eq!(&output[..count], b"buried treasure");
    assert_eq!(*device.0.lock().unwrap(), before);
}

#[test]
fn primitives_survive_missing_boot_and_mft_metadata() {
    let payload = vec![0x63; 12000];
    let (device, number) = populated(&payload);
    let volume = Volume::open_for_salvage(device.clone()).unwrap();
    let record = volume.inspect_mft_record(number).unwrap();
    let mft_offset = volume.boot().mft_lcn * volume.boot().cluster_size;
    device.write_at(0, &[0; 512]).unwrap();
    device.write_at(mft_offset, &[0; 1024]).unwrap();
    assert!(Volume::open(device.clone()).is_err());
    let raw = RawVolume::open(device.clone());
    let before = device.0.lock().unwrap().clone();
    let recovered = raw.read_file_record_at(record.offset, 1024, 512).unwrap();
    let mut output = vec![0; payload.len()];
    assert_eq!(
        raw.read_salvage_stream_at(&recovered, data_attribute(&recovered), 4096, 0, &mut output)
            .unwrap(),
        payload.len()
    );
    assert_eq!(output, payload);
    assert_eq!(*device.0.lock().unwrap(), before);
}

#[test]
fn torn_records_are_preserved_but_not_silently_repaired() {
    let (device, number) = populated(b"kept");
    let volume = Volume::open_for_salvage(device.clone()).unwrap();
    let record = volume.inspect_mft_record(number).unwrap();
    let mut torn = record.raw.clone();
    torn[510] ^= 1;
    device.write_at(record.offset, &torn).unwrap();
    let evidence = volume
        .read_file_record_at(record.offset, 1024, 512)
        .unwrap();
    assert_eq!(evidence.raw, torn);
    assert!(evidence.fixed.is_none());
    assert!(!evidence.issues.is_empty());
    assert!(volume
        .read_salvage_stream_at(&evidence, 0, 4096, 0, &mut [0; 8])
        .is_err());
}

#[test]
fn raw_volume_is_bounded_and_does_not_require_a_filesystem() {
    let device = Memory(Arc::new(Mutex::new(vec![7; 1024])));
    let raw = RawVolume::open(device.clone());
    let mut output = [0; 4];
    assert!(raw.geometry().is_err());
    raw.read_volume_bytes(0, &mut output).unwrap();
    assert_eq!(output, [7; 4]);
    assert!(raw.read_volume_bytes(1022, &mut output).is_err());
    assert!(raw.read_volume_bytes(u64::MAX, &mut output).is_err());
    assert!(raw.read_file_record_at(0, usize::MAX, 512).is_err());
    assert!(raw.read_file_record_at(1024, 512, 512).is_err());
    let writable = RawVolume::open_writable(device).unwrap();
    writable.write_volume_bytes(0, &[8; 4]).unwrap();
    assert!(writable.write_volume_bytes(1024, &[1]).is_err());
    writable.flush().unwrap();
    raw.read_volume_bytes(0, &mut output).unwrap();
    assert_eq!(output, [8; 4]);
    assert!(inspect_file_record(0, vec![], 0).fixed.is_none());
}

#[test]
fn salvage_refuses_transformed_or_incomplete_mapping_pairs() {
    let (device, number) = populated(&vec![3; 12000]);
    let volume = Volume::open_for_salvage(device).unwrap();
    let mut record = volume.inspect_mft_record(number).unwrap();
    let index = data_attribute(&record);
    record.attributes[index].flags = 1;
    assert!(volume
        .read_salvage_stream_at(&record, index, 4096, 0, &mut [0; 16])
        .is_err());
    record.attributes[index].flags = 0;
    if let AttributeValue::NonResident { runs, .. } = &mut record.attributes[index].value {
        runs.clear();
    } else {
        panic!("nonresident fixture");
    }
    assert!(volume
        .read_salvage_stream_at(&record, index, 4096, 0, &mut [0; 16])
        .is_err());
    assert!(volume.cluster_offset(u64::MAX).is_err());
    assert!(volume
        .read_volume_bytes(volume.size_bytes(), &mut [0; 1])
        .is_err());
}

#[test]
fn named_streams_share_the_normal_file_reader() {
    let (mut device, _) = populated(b"default");
    fs_ntfs::write::write_named_stream_io(
        &mut device,
        "/treasure.txt",
        "notes",
        b"stream treasure",
    )
    .unwrap();
    let volume = Volume::open(device).unwrap();
    assert_eq!(
        volume
            .read_stream_at("/treasure.txt", "notes", 7, 100)
            .unwrap(),
        b"treasure"
    );
    assert!(volume
        .read_stream_at("/treasure.txt", "absent", 0, 10)
        .is_err());
}

#[test]
fn primitive_streams_distinguish_sparse_holes_uninitialised_bytes_and_missing_runs() {
    use fs_ntfs::data_runs::DataRun;
    let (device, number) = populated(&vec![0x71; 12000]);
    let volume = Volume::open_for_salvage(device).unwrap();
    let mut record = volume.inspect_mft_record(number).unwrap();
    let index = data_attribute(&record);
    let original_lcn = match &record.attributes[index].value {
        AttributeValue::NonResident { runs, .. } => runs[0].lcn,
        _ => panic!("nonresident fixture"),
    };
    record.attributes[index].value = AttributeValue::NonResident {
        runs: vec![
            DataRun {
                starting_vcn: 0,
                length: 1,
                lcn: original_lcn,
            },
            DataRun {
                starting_vcn: 1,
                length: 2,
                lcn: None,
            },
        ],
        first_vcn: 0,
        data_size: 12000,
        initialized_size: 9000,
    };
    let mut output = vec![0xff; 12000];
    assert_eq!(
        volume
            .read_salvage_stream_at(&record, index, 4096, 0, &mut output)
            .unwrap(),
        12000
    );
    assert_eq!(&output[..4096], vec![0x71; 4096]);
    assert!(output[4096..].iter().all(|byte| *byte == 0));
    assert_eq!(
        volume
            .read_salvage_stream_at(&record, index, 4096, u64::MAX, &mut output)
            .unwrap(),
        0
    );
    assert!(volume
        .read_salvage_stream_at(&record, index, 3, 0, &mut output)
        .is_err());
    if let AttributeValue::NonResident { runs, .. } = &mut record.attributes[index].value {
        runs[1].starting_vcn = 2;
    }
    assert!(volume
        .read_salvage_stream_at(&record, index, 4096, 0, &mut output)
        .is_err());
}

#[test]
fn raw_and_validated_volume_reads_have_different_bounds() {
    let (device, _) = populated(b"kept");
    let volume = Volume::open(device.clone()).unwrap();
    let end = volume.size_bytes();
    assert!(end < device.size_bytes());
    assert!(volume.read_volume_bytes(end, &mut [0; 1]).is_err());
    volume.raw().read_volume_bytes(end, &mut [0; 1]).unwrap();
    assert!(RawVolume::open_writable(fs_core::ReadOnlyDevice::new(device)).is_err());
}

#[test]
fn arbitrary_record_bytes_and_sector_geometries_never_panic() {
    for length in [0, 1, 3, 7, 47, 48, 512, 1024, 4096] {
        for sector in [0, 1, 3, 512, 4096, u16::MAX] {
            let mut bytes = vec![0xff; length];
            if length >= 4 {
                bytes[..4].copy_from_slice(b"FILE");
            }
            let evidence = inspect_file_record(7, bytes.clone(), sector);
            assert_eq!(evidence.raw, bytes);
            assert!(!evidence.issues.is_empty());
        }
    }
}

#[test]
fn attribute_damage_retains_earlier_valid_attributes_and_raw_bytes() {
    let (device, number) = populated(b"kept");
    let volume = Volume::open_for_salvage(device).unwrap();
    let original = volume.inspect_mft_record(number).unwrap();
    let mut bytes = original.fixed.clone().unwrap();
    let data = original.attributes[data_attribute(&original)].offset;
    bytes[data + 4..data + 8].copy_from_slice(&0u32.to_le_bytes());
    fs_ntfs::mft_io::apply_fixup_on_write(&mut bytes, 512).unwrap();
    let damaged = inspect_file_record(original.offset, bytes.clone(), 512);
    assert_eq!(damaged.raw, bytes);
    assert!(!damaged.issues.is_empty());
    assert!(damaged
        .attributes
        .iter()
        .all(|attribute| attribute.type_code != 0x80));
    assert!(damaged
        .attributes
        .iter()
        .any(|attribute| attribute.type_code == 0x30));
}

#[test]
fn writable_mount_refuses_dirty_metadata_without_replaying_or_writing() {
    let mut device = formatted();
    fs_ntfs::fsck::set_dirty_io(&mut device).unwrap();
    let before = device.0.lock().unwrap().clone();
    assert!(Volume::open(device.clone()).is_ok());
    assert!(Volume::open_writable(device.clone()).is_err());
    assert_eq!(*device.0.lock().unwrap(), before);
}

#[test]
fn truncated_images_keep_existing_reader_compatibility_and_bounded_io() {
    let (device, _) = populated(b"kept");
    let size = device.size_bytes();
    device
        .write_at(40, &(size / 512 + 1000).to_le_bytes())
        .unwrap();
    let volume = Volume::open(device.clone()).unwrap();
    assert!(volume.geometry().unwrap().volume_bytes() > size);
    assert_eq!(volume.size_bytes(), size);
    assert_eq!(
        volume.read_stream_at("/treasure.txt", "", 0, 4).unwrap(),
        b"kept"
    );
    assert!(volume.read_volume_bytes(size, &mut [0; 1]).is_err());
    assert!(volume.cluster_offset(size / 4096).is_err());
}

#[test]
fn primitive_streams_accept_large_ntfs_cluster_hypotheses() {
    let (device, number) = populated(&vec![3; 12000]);
    let volume = Volume::open_for_salvage(device.clone()).unwrap();
    let mut record = volume.inspect_mft_record(number).unwrap();
    let index = data_attribute(&record);
    for cluster_size in [128 * 1024, 2 * 1024 * 1024] {
        device.write_at(cluster_size, b"large").unwrap();
        record.attributes[index].value = AttributeValue::NonResident {
            runs: vec![fs_ntfs::data_runs::DataRun {
                starting_vcn: 0,
                length: 1,
                lcn: Some(1),
            }],
            first_vcn: 0,
            data_size: 5,
            initialized_size: 5,
        };
        let mut output = [0; 5];
        assert_eq!(
            volume
                .read_salvage_stream_at(&record, index, cluster_size, 0, &mut output)
                .unwrap(),
            5
        );
        assert_eq!(&output, b"large");
    }
}

#[test]
fn record_evidence_decodes_names_times_and_number_like_the_independent_reader() {
    let (device, number) = populated(&vec![0x42; 9000]);
    let volume = Volume::open_for_salvage(device.clone()).unwrap();
    let record = volume.inspect_mft_record(number).unwrap();
    assert_eq!(record.record_number, Some(number));
    assert!(!record.has_attribute_list());

    let bytes = device.0.lock().unwrap().clone();
    let mut cursor = std::io::Cursor::new(bytes);
    let independent = ntfs::Ntfs::new(&mut cursor).unwrap();
    let file = independent.file(&mut cursor, number).unwrap();
    assert_eq!(file.file_record_number(), number);
    assert_eq!(
        u64::from(file.sequence_number()),
        u64::from(record.sequence)
    );

    let info = file.info().unwrap();
    let decoded = record.standard_information().unwrap();
    assert_eq!(decoded.created_nt, info.creation_time().nt_timestamp());
    assert_eq!(decoded.modified_nt, info.modification_time().nt_timestamp());
    assert_eq!(
        decoded.mft_modified_nt,
        info.mft_record_modification_time().nt_timestamp()
    );
    assert_eq!(decoded.accessed_nt, info.access_time().nt_timestamp());
    assert_eq!(decoded.file_attributes, info.file_attributes().bits());

    let names = record.file_names();
    assert!(!names.is_empty());
    let reference = file
        .name(&mut cursor, None, None)
        .expect("the file has a name")
        .unwrap();
    let expected = names
        .iter()
        .find(|name| name.name == reference.name().to_string_lossy())
        .expect("the independent reader's name was decoded");
    assert!(expected.name_valid);
    assert_eq!(expected.name, "treasure.txt");
    assert_eq!(
        expected.parent_record,
        reference.parent_directory_reference().file_record_number()
    );
    assert_eq!(
        expected.parent_sequence,
        reference.parent_directory_reference().sequence_number()
    );
    assert_eq!(
        expected.created_nt,
        reference.creation_time().nt_timestamp()
    );
    assert_eq!(expected.allocated_size, reference.allocated_size());
    assert_eq!(expected.real_size, reference.data_size());
    assert_eq!(expected.flags, reference.file_attributes().bits());
    assert_eq!(expected.parent_record, 5, "created in the root directory");
}

#[test]
fn record_evidence_decoders_refuse_what_the_attribute_cannot_hold() {
    let (device, number) = populated(b"small");
    let volume = Volume::open_for_salvage(device).unwrap();
    let mut record = volume.inspect_mft_record(number).unwrap();
    for attribute in &mut record.attributes {
        if attribute.type_code == AttrType::FileName as u32 {
            // A name length that runs past the value is skipped, not guessed at.
            if let AttributeValue::Resident(value) = &mut attribute.value {
                value[64] = 255;
            }
        }
        if attribute.type_code == AttrType::StandardInformation as u32 {
            if let AttributeValue::Resident(value) = &mut attribute.value {
                value.truncate(40);
            }
        }
    }
    assert!(record.file_names().is_empty());
    assert!(record.standard_information().is_none());
    let mut unpaired = volume.inspect_mft_record(number).unwrap();
    for attribute in &mut unpaired.attributes {
        if attribute.type_code == AttrType::FileName as u32 {
            if let AttributeValue::Resident(value) = &mut attribute.value {
                value[66..68].copy_from_slice(&0xd800u16.to_le_bytes());
            }
        }
    }
    let name = &unpaired.file_names()[0];
    assert!(!name.name_valid);
    assert!(name.name.contains('\u{fffd}'));
}

#[test]
fn a_boot_sector_in_memory_is_parsed_and_validated_without_a_device() {
    let device = formatted();
    let mut boot = [0u8; 512];
    device.read_at(0, &mut boot).unwrap();
    let parsed = fs_ntfs::mft_io::parse_boot_sector(&boot).unwrap();
    let opened = Volume::open_for_salvage(device).unwrap();
    assert_eq!(parsed.cluster_size, opened.boot().cluster_size);
    assert_eq!(parsed.total_sectors, opened.boot().total_sectors);
    assert_eq!(parsed.serial_number, 0x123456789abcdef0);
    assert!(fs_ntfs::mft_io::parse_boot_sector(&boot[..511]).is_err());
    boot[3] = b'X';
    assert!(fs_ntfs::mft_io::parse_boot_sector(&boot).is_err());
    assert!(fs_ntfs::mft_io::parse_boot_sector(&[0; 512]).is_err());
}

#[test]
fn volume_record_evidence_matches_the_mounted_volume_information() {
    let device = formatted();
    let volume = Volume::open(device).unwrap();
    let info = volume.volume_info().unwrap();
    let record = volume.inspect_mft_record(3).unwrap();
    let decoded = record.volume_information().unwrap();
    assert_eq!(
        (decoded.major, decoded.minor),
        (info.version_major, info.version_minor)
    );
    assert_eq!(decoded.flags, info.flags);
    assert_eq!(decoded.is_dirty(), info.flags & 1 != 0);
    assert_eq!(record.volume_name().unwrap(), info.label);
    assert_eq!(info.label, "THREE API");
    assert!(volume
        .inspect_mft_record(5)
        .unwrap()
        .volume_information()
        .is_none());
}

/// A 1024-byte FILE record with an empty attribute list, whose update sequence array starts at
/// `usa_offset`. NTFS 3.1 starts it at 0x30 and keeps the record's own number at 0x2c; the older
/// layout starts it at 0x2a, so those bytes belong to the array and say nothing about a number.
fn bare_record(usa_offset: u16, number_at_0x2c: Option<u32>) -> Vec<u8> {
    let mut record = vec![0u8; 1024];
    record[0..4].copy_from_slice(b"FILE");
    record[4..6].copy_from_slice(&usa_offset.to_le_bytes());
    record[6..8].copy_from_slice(&3u16.to_le_bytes());
    record[16..18].copy_from_slice(&1u16.to_le_bytes());
    let first = usa_offset.div_ceil(8) * 8 + 8;
    record[20..22].copy_from_slice(&first.to_le_bytes());
    record[22..24].copy_from_slice(&1u16.to_le_bytes());
    record[24..28].copy_from_slice(&(u32::from(first) + 8).to_le_bytes());
    record[28..32].copy_from_slice(&1024u32.to_le_bytes());
    if let Some(number) = number_at_0x2c {
        record[0x2c..0x30].copy_from_slice(&number.to_le_bytes());
    }
    record[usize::from(first)..usize::from(first) + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs_ntfs::mft_io::apply_fixup_on_write(&mut record, 512).unwrap();
    record
}

#[test]
fn a_record_number_is_only_read_where_the_header_has_one() {
    // NTFS 3.1: the update sequence array starts at 0x30 and the number is at 0x2c.
    let modern = inspect_file_record(0, bare_record(0x30, Some(77)), 512);
    assert!(modern.issues.is_empty(), "{:?}", modern.issues);
    assert_eq!(modern.record_number, Some(77));
    // The layout the Linux kernel's ntfs3 driver writes: the array starts at 0x2a, so the bytes
    // at 0x2c are the array's own, and a number read from them would be wrong.
    let legacy = inspect_file_record(0, bare_record(0x2a, None), 512);
    assert!(legacy.issues.is_empty(), "{:?}", legacy.issues);
    assert_eq!(legacy.record_number, None);
}
