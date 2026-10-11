//! A volume Windows hibernated, or shut down with Fast Startup, is refused
//! for writing (#137).
//!
//! Hibernation and Fast Startup save the running kernel, NTFS's cached
//! metadata included, in `hiberfil.sys`, and the next boot resumes from it
//! (Microsoft, "Distinguishing Fast Startup from Wake-from-Hibernation").
//! Windows does not see a change made to the volume in between: it carries
//! on from what it cached, and its next write lands on metadata that no
//! longer matches. The log cannot tell this case from a crash, so replaying
//! it is no answer either. A volume in this state is not written to: no
//! read-write mount, and no `fsck`.
//!
//! THE ORACLE IS NOT THIS CRATE. ntfs-3g, run at arm's length (GPL, never
//! linked), refuses a read-write mount of such a volume: `ntfs-3g.probe
//! --readwrite` exits 14, "Windows is hibernated, refused to mount", when
//! `hiberfil.sys` in the root begins with `hibr` or `HIBR`, or is shorter
//! than its 4 KiB header. A header of zeros it opens. Every volume below is
//! graded by ntfs-3g before this crate sees it, so a header this test gets
//! wrong fails there rather than agreeing with us. Missing, the test FAILS
//! naming the package.

mod common;

use std::ffi::CString;
use std::path::Path;
use std::process::Command;

use fs_ntfs::block_io::PathIo;
use fs_ntfs::facade::Filesystem;
use fs_ntfs::fsck;
use fs_ntfs::mkfs::format_filesystem;
use fs_ntfs::{fs_ntfs_mount, fs_ntfs_umount};

const SIZE: u64 = 64 << 20;

/// A volume this crate formatted, holding `/hiberfil.sys` with `contents`.
///
/// The file is written under another name and then renamed. Created empty
/// as `hiberfil.sys`, it is shorter than its header, which is a volume
/// Windows is hibernated on, and every write that would fill it is refused.
fn volume_with_hiberfil(stem: &str, contents: &[u8]) -> String {
    let img = common::temp_image_path(stem);
    std::fs::File::create(&img)
        .and_then(|f| f.set_len(SIZE))
        .expect("create image");
    let mut dev = PathIo::open_rw(Path::new(&img)).expect("open image");
    format_filesystem(&mut dev, SIZE, 4096, 4096, None, Some(1)).expect("format");
    drop(dev);
    let fs = Filesystem::mount_rw(&img).expect("mount the fresh volume");
    fs.create_file("/", "hiberfil.new")
        .expect("create hiberfil.new");
    fs.write_file_contents("/hiberfil.new", contents)
        .expect("write hiberfil.new");
    fs.rename("/hiberfil.new", "hiberfil.sys")
        .expect("rename hiberfil.new to hiberfil.sys");
    img
}

/// A `hiberfil.sys` of `len` bytes whose header begins with `signature`.
fn hiberfil(signature: &[u8; 4], len: usize) -> Vec<u8> {
    let mut contents = vec![0u8; len];
    contents[..4].copy_from_slice(signature);
    contents
}

/// `ntfs-3g.probe --readwrite`: its exit status and stderr. 0 is
/// "mountable read-write"; 14 is ntfs-3g's refusal of a hibernated or
/// unclean volume, which its message tells apart.
fn ntfs3g_probe_rw(img: &str) -> (i32, String) {
    let out = Command::new("ntfs-3g.probe")
        .args(["--readwrite", img])
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "ntfs-3g.probe could not be run ({e}): install ntfs-3g (`apt-get install ntfs-3g`)"
            )
        });
    (
        out.status.code().expect("exit status"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn last_error() -> String {
    let p = fs_ntfs::fs_ntfs_last_error();
    if p.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

/// Every way this crate writes to a whole volume, each over an image path:
/// `Ok` once it ran, or the reason it refused.
type VolumeWrite = fn(&str) -> Result<(), String>;

const WRITES: [(&str, VolumeWrite); 3] = [
    ("Filesystem::mount_rw", |img| {
        Filesystem::mount_rw(img).map(|_| ()).map_err(|e| e.0)
    }),
    ("fs_ntfs_mount", |img| {
        let c_path = CString::new(img).unwrap();
        let h = fs_ntfs_mount(c_path.as_ptr());
        if h.is_null() {
            return Err(last_error());
        }
        fs_ntfs_umount(h);
        Ok(())
    }),
    ("fsck::fsck", |img| {
        fsck::fsck(img).map(|_| ()).map_err(|e| e.to_string())
    }),
];

/// What `hiberfil.sys` holds, and whether ntfs-3g opens such a volume
/// read-write.
const CASES: [(&str, [u8; 4], usize, bool); 4] = [
    ("hibernated", *b"hibr", 64 << 10, false),
    ("hibernated, Windows 8 and later", *b"HIBR", 64 << 10, false),
    ("shorter than its header", [0; 4], 1024, false),
    ("resumed: a header of zeros", [0; 4], 64 << 10, true),
];

#[test]
fn a_volume_windows_hibernated_is_refused_for_writing_as_ntfs_3g_refuses_it() {
    for (case, signature, len, writable) in CASES {
        let stem = format!(
            "hibernated_{}",
            case.replace(|c: char| !c.is_alphanumeric(), "_")
        );
        let probe = volume_with_hiberfil(&stem, &hiberfil(&signature, len));
        let (code, err) = ntfs3g_probe_rw(&probe);
        if writable {
            assert_eq!(code, 0, "{case}: ntfs-3g refuses it read-write: {err}");
        } else {
            assert!(
                code == 14 && err.contains("ibernated"),
                "{case}: ntfs-3g.probe --readwrite exited {code}, not 14 for a hibernated \
                 volume: {err}"
            );
        }
        std::fs::remove_file(&probe).unwrap();

        for (how, write) in WRITES {
            let img = volume_with_hiberfil(&stem, &hiberfil(&signature, len));
            let before = std::fs::read(&img).unwrap();
            match (write(&img), writable) {
                (Ok(()), true) => {}
                (Err(why), true) => panic!("{case}: {how} refused a volume ntfs-3g opens: {why}"),
                (Ok(()), false) => panic!("{case}: {how} wrote to a volume Windows hibernated"),
                (Err(why), false) => {
                    assert!(
                        why.contains("ibernat"),
                        "{case}: {how} refused, but not for the hibernation: {why}"
                    );
                    assert!(
                        std::fs::read(&img).unwrap() == before,
                        "{case}: {how} refused and wrote to the volume anyway"
                    );
                }
            }
            std::fs::remove_file(&img).unwrap();
        }
    }
}
