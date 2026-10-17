//! Contract for the independent expected-state model, without a disk or driver.
#[path = "op_sequence_model/model.rs"]
mod model;

use model::{Error, Model, MAX_FILE_BYTES};

fn table() -> Vec<u16> {
    let mut table: Vec<_> = (0..=u16::MAX).collect();
    for c in b'a'..=b'z' {
        table[c as usize] = c.to_ascii_uppercase() as u16;
    }
    table
}

fn tree() -> Model {
    Model::new(table()).unwrap()
}

#[test]
fn offset_writes_and_truncates_define_logical_bytes() {
    let mut m = tree();
    m.create("/", "file", false).unwrap();
    m.write("/file", 3, b"abc").unwrap();
    m.write("/file", 4, b"XY").unwrap();
    assert_eq!(m.read("/file").unwrap(), b"\0\0\0aXY");
    m.truncate("/file", 4).unwrap();
    m.truncate("/file", 8).unwrap();
    assert_eq!(m.read("/file").unwrap(), b"\0\0\0a\0\0\0\0");
    m.write("/file", usize::MAX, b"").unwrap();
    assert_eq!(m.stat("/file").unwrap().size, 8);
    m.truncate("/file", 0).unwrap();
    assert_eq!(m.read("/file").unwrap(), b"");
}

#[test]
fn renaming_a_directory_keeps_its_children_and_identity() {
    let mut m = tree();
    let dir = m.create("/", "dir", true).unwrap();
    let file = m.create("/dir", "child", false).unwrap();
    m.write("/dir/child", 0, b"kept").unwrap();
    m.rename("/dir", "renamed", false).unwrap();
    assert_eq!(m.stat("/renamed").unwrap().id, dir);
    assert!(m.stat("/renamed").unwrap().directory);
    assert_eq!(m.list("/renamed").unwrap(), vec![("child".into(), file)]);
    assert_eq!(m.read("/renamed/child").unwrap(), b"kept");
    assert_eq!(m.stat("/dir"), Err(Error::Missing));
    m.remove("/renamed/child").unwrap();
    m.remove("/renamed").unwrap();
    assert!(m.list("/").unwrap().is_empty());
}

#[test]
fn hard_links_share_bytes_size_and_lifetime() {
    let mut m = tree();
    let id = m.create("/", "first", false).unwrap();
    m.create("/", "dir", true).unwrap();
    m.link("/first", "/dir", "second").unwrap();
    m.write("/dir/second", 0, b"shared").unwrap();
    assert_eq!(m.read("/first").unwrap(), b"shared");
    assert_eq!(m.stat("/first").unwrap().links, 2);
    assert_eq!(m.stat("/dir/second").unwrap().id, id);
    m.truncate("/first", 2).unwrap();
    assert_eq!(m.stat("/dir/second").unwrap().size, 2);
    m.remove("/first").unwrap();
    assert_eq!(m.stat("/dir/second").unwrap().links, 1);
    assert_eq!(m.read("/dir/second").unwrap(), b"sh");
    m.remove("/dir/second").unwrap();
    assert_ne!(m.create("/", "first", false).unwrap(), id);
}

#[test]
fn namespace_keys_follow_the_supplied_utf16_table() {
    let mut supplied = table();
    // Deliberately synthetic: the model must use injected data, not Unicode rules.
    supplied['µ' as usize] = 'X' as u16;
    let mut m = Model::new(supplied).unwrap();
    let id = m.create("/", "µ", false).unwrap();
    assert_eq!(m.stat("/x").unwrap().id, id);
    assert_eq!(m.create("/", "X", false), Err(Error::Exists));
    m.rename("/µ", "x", false).unwrap();
    assert_eq!(m.list("/").unwrap(), vec![("x".into(), id)]);
    m.create("/", "ß", false).unwrap();
    m.create("/", "SS", false).unwrap();
    m.create("/", &"😀".repeat(127), false).unwrap();
    m.create("/", &"n".repeat(255), false).unwrap();
}

#[test]
fn namespace_refusals_leave_the_entire_model_unchanged() {
    let mut m = tree();
    m.create("/", "dir", true).unwrap();
    m.create("/dir", "file", false).unwrap();
    m.create("/", "other", false).unwrap();
    let before = m.clone();
    assert_eq!(m.create("/DIR", "FILE", false), Err(Error::Exists));
    assert_eq!(m.create("/missing", "file", false), Err(Error::Missing));
    assert_eq!(m.create("/other", "file", false), Err(Error::NotDirectory));
    assert_eq!(m.link("/dir", "/", "alias"), Err(Error::IsDirectory));
    assert_eq!(m.link("/other", "/dir", "file"), Err(Error::Exists));
    assert_eq!(m.rename("/other", "dir", false), Err(Error::Exists));
    assert_eq!(m.remove("/dir"), Err(Error::NotEmpty));
    assert_eq!(m.remove("/"), Err(Error::InvalidPath));
    assert_eq!(m.remove("/missing"), Err(Error::Missing));
    assert_eq!(m, before);
}

#[test]
fn invalid_paths_names_and_sizes_are_atomic_refusals() {
    let mut m = tree();
    m.create("/", "file", false).unwrap();
    let before = m.clone();
    for name in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "a:b",
        &"😀".repeat(128),
    ] {
        assert_eq!(m.create("/", name, false), Err(Error::InvalidName));
    }
    for path in ["file", "//file", "/file/", "/./file", "/../file"] {
        assert_eq!(m.write(path, 0, b"x"), Err(Error::InvalidPath));
    }
    assert_eq!(m.write("/file", usize::MAX, b"x"), Err(Error::FileTooLarge));
    assert_eq!(
        m.truncate("/file", MAX_FILE_BYTES + 1),
        Err(Error::FileTooLarge)
    );
    assert_eq!(m.write("/", 0, b"x"), Err(Error::IsDirectory));
    assert_eq!(m.read("/"), Err(Error::IsDirectory));
    assert_eq!(m.list("/file"), Err(Error::NotDirectory));
    assert_eq!(m, before);
}

#[test]
fn replacement_rename_handles_aliases_and_directory_constraints() {
    let mut m = tree();
    m.create("/", "a", false).unwrap();
    m.write("/a", 0, b"a").unwrap();
    m.create("/", "b", false).unwrap();
    m.link("/a", "/", "alias").unwrap();
    let before = m.clone();
    m.rename("/a", "alias", true).unwrap();
    assert_eq!(m, before);
    m.rename("/a", "b", true).unwrap();
    assert_eq!(m.read("/b").unwrap(), b"a");
    assert_eq!(m.stat("/b").unwrap().links, 2);
    assert_eq!(m.stat("/a"), Err(Error::Missing));
    m.create("/", "dir", true).unwrap();
    m.create("/", "empty", true).unwrap();
    let before = m.clone();
    assert_eq!(m.rename("/b", "dir", true), Err(Error::IsDirectory));
    assert_eq!(m.rename("/dir", "b", true), Err(Error::NotDirectory));
    assert_eq!(m, before);
    m.create("/dir", "child", false).unwrap();
    let nonempty = m.clone();
    assert_eq!(m.rename("/empty", "dir", true), Err(Error::NotEmpty));
    assert_eq!(m, nonempty);
    m.remove("/dir/child").unwrap();
    let id = m.stat("/dir").unwrap().id;
    m.rename("/dir", "empty", true).unwrap();
    assert_eq!(m.stat("/empty").unwrap().id, id);
}

#[test]
fn an_incomplete_upcase_table_is_refused() {
    assert_eq!(Model::new(vec![]), Err(Error::InvalidTable));
    assert_eq!(Model::new(vec![0; 65535]), Err(Error::InvalidTable));
    assert_eq!(Model::new(vec![0; 65537]), Err(Error::InvalidTable));
}

#[test]
fn named_streams_belong_to_the_file_and_fold_through_the_supplied_table() {
    let mut supplied = table();
    // σ and Σ name one stream only because the supplied table says so.
    supplied[0x3c3] = 0x3a3;
    let mut m = Model::new(supplied).unwrap();
    m.create("/", "f", false).unwrap();
    m.create("/", "d", true).unwrap();
    m.write_stream("/f", "s", b"one").unwrap();
    m.write_stream("/f", "S", b"two").unwrap();
    m.write_stream("/f", "σ", b"").unwrap();
    m.write_stream("/f", "Σ", b"sigma").unwrap();
    // A colliding name replaces the bytes and keeps the first spelling.
    assert_eq!(
        m.streams("/f").unwrap(),
        vec![
            ("s".into(), b"two".to_vec()),
            ("σ".into(), b"sigma".to_vec())
        ]
    );
    // The unnamed data is not a stream, and streams do not count in its size.
    assert_eq!(m.read("/f").unwrap(), b"");
    assert_eq!(m.stat("/f").unwrap().size, 0);
    // Streams belong to the file: every hard link shows and changes them.
    m.link("/f", "/d", "g").unwrap();
    m.delete_stream("/d/g", "S").unwrap();
    assert_eq!(
        m.streams("/f").unwrap(),
        vec![("σ".into(), b"sigma".to_vec())]
    );

    let before = m.clone();
    let long = "n".repeat(256);
    for name in [
        "", ".", "..", "a:b", "a/b", "a\\b", "x.", "x ", "\u{1}", &long,
    ] {
        assert_eq!(
            m.write_stream("/f", name, b"x"),
            Err(Error::InvalidName),
            "{name:?}"
        );
    }
    assert_eq!(m.write_stream("/missing", "s", b"x"), Err(Error::Missing));
    assert_eq!(m.write_stream("/f/x", "s", b"x"), Err(Error::NotDirectory));
    assert_eq!(
        m.write_stream("/f", "s", &vec![0; MAX_FILE_BYTES + 1]),
        Err(Error::FileTooLarge)
    );
    assert_eq!(m.delete_stream("/f", "s"), Err(Error::Missing));
    assert_eq!(m.delete_stream("/f", ""), Err(Error::InvalidName));
    assert_eq!(m.streams("/missing"), Err(Error::Missing));
    assert_eq!(m, before);

    // 255 UTF-16 units is the longest name, counted in units, not chars.
    let widest = "😀".repeat(127) + "x";
    m.write_stream("/f", &widest, b"").unwrap();
    assert_eq!(m.streams("/d/g").unwrap().len(), 2);
    // A directory holds streams as a file does, and they go with it.
    m.write_stream("/d", "s", b"dir").unwrap();
    assert_eq!(
        m.streams("/d").unwrap(),
        vec![("s".into(), b"dir".to_vec())]
    );
    m.remove("/d/g").unwrap();
    m.remove("/d").unwrap();
    // The last name takes the file's streams with it.
    m.remove("/f").unwrap();
    m.create("/", "f", false).unwrap();
    assert!(m.streams("/f").unwrap().is_empty());
}
