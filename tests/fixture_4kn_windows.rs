//! Windows' own NTFS format of a disk presenting 4096-byte logical sectors
//! (#1150).
//!
//! `test-disks/build-windows-4kn-fixture.ps1` makes a VHDX whose logical
//! sectors are 4096 bytes, has Windows format a partition on it, writes the
//! `ntfs-cli-read.img` file set and copies the volume out to
//! `ntfs-4kn-read.img`. Beside it are `ntfs-4kn-read.ntfsinfo.txt`, what
//! `fsutil fsinfo ntfsinfo` reported for the mounted volume, and
//! `ntfs-4kn-read.sha256.tsv`, the size and SHA-256 `Get-FileHash` recorded for
//! every file. This test holds the copied volume to fsutil's account of it:
//! the image's boot sector names the sector size Windows reported, and that
//! size is 4096.
#![cfg(windows)]

use fs_ntfs::block_io::PathIo;
use fs_ntfs::mft_io::read_boot_params_io;

const IMAGE: &str = "test-disks/ntfs-4kn-read.img";
const NTFSINFO: &str = "test-disks/ntfs-4kn-read.ntfsinfo.txt";
const RECORD: &str = "test-disks/ntfs-4kn-read.sha256.tsv";
const PROVIDER: &str = "test-disks/build-windows-4kn-fixture.ps1";

/// The value fsutil printed for `field`, as `Field Name :  value`.
fn ntfsinfo_field(info: &str, field: &str) -> u64 {
    let line = info
        .lines()
        .find(|l| l.split(':').next().map(str::trim) == Some(field))
        .unwrap_or_else(|| panic!("{NTFSINFO} has no {field:?} line:\n{info}"));
    let value = line.split_once(':').map(|(_, v)| v.trim()).unwrap_or("");
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("{NTFSINFO}: {line:?} is not a number: {e}"))
}

#[test]
fn windows_formats_the_4kn_fixture_in_4096_byte_sectors() {
    let info = std::fs::read_to_string(NTFSINFO)
        .unwrap_or_else(|e| panic!("{NTFSINFO}: {e}; run {PROVIDER} to make it"));
    let reported = ntfsinfo_field(&info, "Bytes Per Sector");
    assert_eq!(reported, 4096, "fsutil reports {reported}-byte sectors");

    let mut io = PathIo::open_ro(IMAGE.as_ref())
        .unwrap_or_else(|e| panic!("{IMAGE}: {e}; run {PROVIDER} to make it"));
    let boot = read_boot_params_io(&mut io).expect("read the boot sector Windows wrote");
    assert_eq!(
        u64::from(boot.bytes_per_sector),
        reported,
        "the copied volume's boot sector disagrees with fsutil"
    );
    let size = std::fs::metadata(IMAGE).expect("stat the image").len();
    assert_eq!(size % 4096, 0, "{IMAGE} is not whole 4096-byte sectors");

    let record = std::fs::read_to_string(RECORD)
        .unwrap_or_else(|e| panic!("{RECORD}: {e}; run {PROVIDER} to make it"));
    let mut files = 0;
    for (n, line) in record.lines().filter(|l| !l.is_empty()).enumerate() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(
            fields.len() == 3 && fields[0].starts_with('/') && fields[2].len() == 64,
            "{RECORD} line {}: {line:?} is not /path<TAB>size<TAB>sha256",
            n + 1
        );
        files += 1;
    }
    assert!(files > 0, "{RECORD} records no files");
}
