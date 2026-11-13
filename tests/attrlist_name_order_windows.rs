//! The order Windows writes same-type `$ATTRIBUTE_LIST` entries in (#1047).
//! The names separate ordinal UTF-16, upcase-folded, prefix and code-point
//! ordering, so the recorded order shows which one Windows uses. Nothing here
//! judges that order: chkdsk grades inversions (#1048) and the checker follows
//! the measured policy (#1049), so directory-name collation is not assumed.
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::PathIo;
use fs_ntfs::{read, write};
use std::path::Path;

const IMAGE: &str = "test-disks/ntfs-attrlist-names.img";
const HANDED: &str = "test-disks/ntfs-attrlist-names.names.tsv";

fn hex(units: &[u16]) -> String {
    units
        .iter()
        .map(|u| format!("{u:04x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn windows_lists_every_handed_stream_name_and_its_order_is_recorded() {
    assert!(
        Path::new(IMAGE).exists() && Path::new(HANDED).exists(),
        "run test-disks/build-windows-native-read-fixtures.ps1 on Windows"
    );
    let mut handed: Vec<String> = std::fs::read_to_string(HANDED)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    assert!(handed.len() > 96, "the padding alone is 96 names");

    let mut io = PathIo::open_ro(Path::new(IMAGE)).unwrap();
    let base = write::resolve_path_to_record_number_io(&mut io, "/names.bin").unwrap();
    let list = read::read_attribute_value(&mut io, base, AttrType::AttributeList, None).unwrap();
    let entries = read::parse_attribute_list(&list).unwrap();

    let mut rows = vec!["index\ttype\tname_utf16\tname\tstarting_vcn\trecord".to_owned()];
    let mut listed = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let units: Vec<u16> = entry
            .name
            .as_deref()
            .map(|name| name.encode_utf16().collect())
            .unwrap_or_default();
        rows.push(format!(
            "{index}\t{:#x}\t{}\t{}\t{}\t{}",
            entry.type_code,
            hex(&units),
            entry.name.as_deref().unwrap_or(""),
            entry.starting_vcn,
            entry.record_number
        ));
        if entry.type_code == AttrType::Data as u32 && entry.starting_vcn == 0 && !units.is_empty()
        {
            listed.push(hex(&units));
        }
    }
    if let Some(dir) = std::env::var_os("NTFS_ATTRLIST_NAME_ORDER") {
        std::fs::create_dir_all(&dir).unwrap();
        let out = Path::new(&dir).join("observed-order.tsv");
        std::fs::write(out, rows.join("\n") + "\n").unwrap();
    }

    // Every name Windows was handed is listed once: a pair Windows folded into
    // one stream would leave the measurement without the case it was for.
    handed.sort();
    listed.sort();
    assert_eq!(listed, handed, "{}", rows.join("\n"));
}
