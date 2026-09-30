//! Opening a target: an image file or a device, read-only or writable,
//! optionally `--offset` bytes in (a partition inside a whole-disk image).

use std::ffi::OsStr;
use std::path::Path;

use fs_core::cli::CliError;
use fs_ntfs::block_io::{BlockIo, PathIo};

/// A target, `offset` bytes into a file or device. Every position the
/// library asks for is checked against the part of the target after the
/// offset, so a volume that claims to run past the end of its partition
/// fails there instead of reading the next one.
pub struct Device {
    inner: PathIo,
    offset: u64,
    size: u64,
}

impl Device {
    fn at(&self, pos: u64, len: usize) -> Result<u64, String> {
        let end = pos
            .checked_add(len as u64)
            .ok_or_else(|| format!("I/O at {pos} + {len} overflows"))?;
        if end > self.size {
            return Err(format!(
                "I/O at {pos}..{end} runs past the end of the target ({} bytes)",
                self.size
            ));
        }
        Ok(self.offset + pos)
    }
}

impl BlockIo for Device {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let at = self.at(offset, buf.len())?;
        self.inner.read_exact_at(at, buf)
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let at = self.at(offset, buf.len())?;
        self.inner.write_all_at(at, buf)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn sync(&mut self) -> Result<(), String> {
        self.inner.sync()
    }
}

/// Open `target`, `offset` bytes in, read-only or read-write.
pub fn open(target: &OsStr, offset: u64, writable: bool) -> Result<Device, CliError> {
    let name = target.to_string_lossy();
    let path = Path::new(target);
    let inner = if writable {
        PathIo::open_rw(path)
    } else {
        PathIo::open_ro(path)
    }
    .map_err(CliError::failed)?;
    let total = inner.size();
    if offset > 0 && offset >= total {
        return Err(CliError::failed(format!(
            "--offset {offset} is past the end of {name} ({total} bytes)"
        )));
    }
    Ok(Device {
        inner,
        offset,
        size: total - offset,
    })
}

/// Open `target` and check it is an NTFS volume: the boot sector, the MFT
/// and `$Volume` must read. Returns the device and what `$Volume` says.
pub fn mount(
    target: &OsStr,
    offset: u64,
    writable: bool,
) -> Result<(Device, fs_ntfs::read::VolumeInfo), CliError> {
    let mut dev = open(target, offset, writable)?;
    let info = fs_ntfs::read::read_volume_info(&mut dev).map_err(|e| {
        CliError::failed(format!(
            "{} is not a readable NTFS filesystem: {e}",
            target.to_string_lossy()
        ))
    })?;
    Ok((dev, info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_past_the_end_is_refused_and_io_stays_inside_the_window() {
        let dir = std::env::temp_dir().join(format!("fs-ntfs-cli-device-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("window.img");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        std::fs::write(&file, &bytes).unwrap();

        assert!(open(file.as_os_str(), 4096, false).is_err());
        let mut dev = open(file.as_os_str(), 1024, false).expect("open at 1024");
        assert_eq!(dev.size(), 3072);
        let mut buf = [0u8; 4];
        dev.read_exact_at(0, &mut buf).unwrap();
        assert_eq!(buf, [bytes[1024], bytes[1025], bytes[1026], bytes[1027]]);
        assert!(
            dev.read_exact_at(3070, &mut buf).is_err(),
            "reads past the window"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
