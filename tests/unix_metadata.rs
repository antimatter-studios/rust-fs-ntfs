//! Linux owner, group and mode in WSL's `$LXUID`, `$LXGID` and `$LXMOD`
//! (#1195, part of #599), through the write API on a fresh volume.
//!
//! These prove the API keeps what it is given and refuses what it cannot
//! store. Whether the bytes are WSL's is graded by Linux's ntfs3 in
//! `tests/kernel_ntfs3.rs`.

mod common;

use std::path::Path;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::error::Kind;
use fs_ntfs::facade::Filesystem;
use fs_ntfs::unix_metadata::UnixMetadata;
use fs_ntfs::{mkfs, write};

const VOLUME_BYTES: u64 = 16 << 20;

/// A fresh volume holding `/file`, with another EA, and `/dir`.
fn volume(name: &str) -> String {
    let img = common::temp_image_path(format!("unix_metadata_{name}"));
    std::fs::File::create(&img)
        .and_then(|f| f.set_len(VOLUME_BYTES))
        .expect("create image");
    let mut io = PathIo::open_rw(Path::new(&img)).expect("open");
    mkfs::format_filesystem(&mut io, VOLUME_BYTES, 4096, 1024, None, Some(1)).expect("mkfs");
    write::create_file_io(&mut io, "/", "file").expect("create");
    write::write_ea_io(&mut io, "/file", b"USER.KEEP", b"kept", 0).expect("ea");
    write::mkdir_io(&mut io, "/", "dir").expect("mkdir");
    img
}

fn meta(uid: Option<u32>, gid: Option<u32>, mode: Option<u32>) -> UnixMetadata {
    UnixMetadata { uid, gid, mode }
}

#[test]
fn owner_group_and_mode_read_back_as_written() {
    let img = volume("round_trip");
    let p = Path::new(&img);
    assert_eq!(
        write::read_unix_metadata(p, "/file").unwrap(),
        meta(None, None, None)
    );

    write::set_unix_metadata(p, "/file", &meta(Some(1000), Some(100), Some(0o640))).unwrap();
    write::set_unix_metadata(p, "/dir", &meta(Some(0), Some(0), Some(0o1777))).unwrap();

    assert_eq!(
        write::read_unix_metadata(p, "/file").unwrap(),
        meta(Some(1000), Some(100), Some(0o100_640))
    );
    assert_eq!(
        write::read_unix_metadata(p, "/dir").unwrap(),
        meta(Some(0), Some(0), Some(0o041_777))
    );
    let fs = Filesystem::mount(&img).unwrap();
    assert_eq!(
        fs.unix_metadata("/file").unwrap(),
        meta(Some(1000), Some(100), Some(0o100_640))
    );
}

#[test]
fn the_values_are_stored_as_wsl_names_them() {
    let img = volume("names");
    let p = Path::new(&img);
    write::set_unix_metadata(
        p,
        "/file",
        &meta(Some(0x0102_0304), Some(7), Some(0o100_600)),
    )
    .unwrap();
    let eas = write::list_eas(p, "/file").unwrap();
    let value = |name: &[u8]| {
        eas.iter()
            .find(|ea| ea.name == name)
            .unwrap_or_else(|| panic!("{} missing", String::from_utf8_lossy(name)))
            .clone()
    };
    assert_eq!(value(b"$LXUID").value, [4, 3, 2, 1]);
    assert_eq!(value(b"$LXGID").value, 7u32.to_le_bytes());
    assert_eq!(value(b"$LXMOD").value, 0o100_600u32.to_le_bytes());
    assert!(eas
        .iter()
        .filter(|ea| ea.name.starts_with(b"$LX"))
        .all(|ea| ea.flags == 0));
    assert_eq!(value(b"USER.KEEP").value, b"kept", "another EA is kept");
}

#[test]
fn an_unset_field_keeps_its_value() {
    let img = volume("partial");
    let p = Path::new(&img);
    write::set_unix_metadata(p, "/file", &meta(Some(1), Some(2), Some(0o600))).unwrap();
    write::set_unix_metadata(p, "/file", &meta(None, Some(20), None)).unwrap();
    assert_eq!(
        write::read_unix_metadata(p, "/file").unwrap(),
        meta(Some(1), Some(20), Some(0o100_600))
    );
}

#[test]
fn the_native_security_descriptor_is_untouched() {
    let img = volume("descriptor");
    let p = Path::new(&img);
    let before = write::read_security_id(p, "/file").unwrap();
    write::set_unix_metadata(p, "/file", &meta(Some(1000), Some(1000), Some(0o600))).unwrap();
    assert_eq!(write::read_security_id(p, "/file").unwrap(), before);
}

#[test]
fn a_mode_that_cannot_be_stored_is_refused_and_nothing_is_written() {
    let img = volume("refused");
    let p = Path::new(&img);
    let cases: [(&str, u32, Kind); 5] = [
        ("/file", 0o040_755, Kind::Invalid),
        ("/dir", 0o100_644, Kind::Invalid),
        ("/file", 0o020_644, Kind::Refused),
        ("/file", 0o010_644, Kind::Refused),
        ("/file", 0o1_000_644, Kind::Invalid),
    ];
    for (path, mode, kind) in cases {
        let err = write::set_unix_metadata(p, path, &meta(Some(5), None, Some(mode))).unwrap_err();
        assert_eq!(err.kind(), kind, "{path} mode {mode:#o}: {}", err.message());
        assert_eq!(
            write::read_unix_metadata(p, path).unwrap(),
            meta(None, None, None),
            "{path} mode {mode:#o}: nothing is written when the mode is refused"
        );
    }
    assert_eq!(
        write::set_unix_metadata(p, "/file", &meta(None, None, None))
            .unwrap_err()
            .kind(),
        Kind::Invalid
    );
}

#[test]
fn a_mode_on_a_reparse_point_is_refused() {
    let img = volume("reparse");
    let p = Path::new(&img);
    write::create_symlink(p, "/", "link", "file", true).unwrap();
    let err = write::set_unix_metadata(p, "/link", &meta(None, None, Some(0o777))).unwrap_err();
    assert_eq!(err.kind(), Kind::Refused, "{}", err.message());
    write::set_unix_metadata(p, "/link", &meta(Some(9), Some(9), None)).unwrap();
}

#[test]
fn a_value_of_the_wrong_length_is_refused_by_name() {
    let img = volume("short");
    let p = Path::new(&img);
    write::write_ea(p, "/file", b"$LXGID", &[1, 0], 0).unwrap();
    let err = write::read_unix_metadata(p, "/file").unwrap_err();
    assert!(err.message().contains("$LXGID"), "{}", err.message());
}
