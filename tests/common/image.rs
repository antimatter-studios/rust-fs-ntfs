use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::IMAGE_CHUNK_SIZE;

#[derive(Debug, PartialEq, Eq)]
pub enum ImageDifference {
    Length { left: u64, right: u64 },
    Byte { offset: u64, left: u8, right: u8 },
}

impl ImageDifference {
    pub fn offset(&self) -> u64 {
        match *self {
            Self::Length { left, right } => left.min(right),
            Self::Byte { offset, .. } => offset,
        }
    }
}

impl std::fmt::Display for ImageDifference {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Length { left, right } => write!(
                output,
                "lengths differ: {left} versus {right} bytes; first missing byte at {}",
                self.offset()
            ),
            Self::Byte {
                offset,
                left,
                right,
            } => write!(
                output,
                "first differing byte at 0x{offset:x}: 0x{left:02x} versus 0x{right:02x}"
            ),
        }
    }
}

pub fn first_image_difference(
    left: impl AsRef<Path>,
    right: impl AsRef<Path>,
) -> io::Result<Option<ImageDifference>> {
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let left_length = left.metadata()?.len();
    let right_length = right.metadata()?.len();
    if left_length != right_length {
        return Ok(Some(ImageDifference::Length {
            left: left_length,
            right: right_length,
        }));
    }
    compare_image_prefix(&mut left, &mut right, left_length)
}

pub fn first_image_difference_in_prefix(
    left: impl AsRef<Path>,
    right: impl AsRef<Path>,
    length: u64,
) -> io::Result<Option<ImageDifference>> {
    compare_image_prefix(&mut File::open(left)?, &mut File::open(right)?, length)
}

fn compare_image_prefix(
    left: &mut File,
    right: &mut File,
    common: u64,
) -> io::Result<Option<ImageDifference>> {
    let mut left_buf = vec![0; IMAGE_CHUNK_SIZE];
    let mut right_buf = vec![0; IMAGE_CHUNK_SIZE];
    let mut offset = 0;
    while offset < common {
        let count = (common - offset).min(IMAGE_CHUNK_SIZE as u64) as usize;
        left.read_exact(&mut left_buf[..count])?;
        right.read_exact(&mut right_buf[..count])?;
        if let Some(index) = left_buf[..count]
            .iter()
            .zip(&right_buf[..count])
            .position(|(left, right)| left != right)
        {
            return Ok(Some(ImageDifference::Byte {
                offset: offset + index as u64,
                left: left_buf[index],
                right: right_buf[index],
            }));
        }
        offset += count as u64;
    }
    Ok(None)
}

#[track_caller]
pub fn assert_images_equal(left: impl AsRef<Path>, right: impl AsRef<Path>, context: &str) {
    if let Some(difference) = first_image_difference(left, right).expect("compare images") {
        panic!("{context}: {difference}");
    }
}

#[track_caller]
pub fn assert_image_unchanged(image: impl AsRef<Path>, before: &ImageSnapshot, context: &str) {
    assert_images_equal(image, before.path(), context);
}

pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    Ok(image_digest(path)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub fn image_digest(path: impl AsRef<Path>) -> io::Result<[u8; 32]> {
    image_digest_from(path, 0)
}

pub fn image_digest_from(path: impl AsRef<Path>, offset: u64) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; IMAGE_CHUNK_SIZE];
    loop {
        let count = match file.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(digest.finalize().into());
        }
        digest.update(&buffer[..count]);
    }
}

pub fn read_image_region(
    path: impl AsRef<Path>,
    offset: u64,
    length: usize,
) -> io::Result<Vec<u8>> {
    if length > IMAGE_CHUNK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "image region exceeds 1 MiB",
        ));
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub fn write_image_region(path: impl AsRef<Path>, offset: u64, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(bytes)
}

pub fn copy_sparse(source: impl AsRef<Path>, destination: impl AsRef<Path>) -> io::Result<()> {
    sparse_copy_impl(source.as_ref(), destination.as_ref(), true)
}

pub fn copy_sparse_portable(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> io::Result<()> {
    sparse_copy_impl(source.as_ref(), destination.as_ref(), false)
}

fn sparse_copy_impl(source: &Path, destination: &Path, native: bool) -> io::Result<()> {
    #[cfg(not(unix))]
    let _ = native;
    #[cfg(target_os = "macos")]
    if native {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(source.as_os_str().as_bytes())?;
        let to = CString::new(destination.as_os_str().as_bytes())?;
        if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Ok(());
        }
    }
    let mut source = File::open(source)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    #[cfg(windows)]
    mark_sparse(&destination)?;
    #[cfg(target_os = "linux")]
    if native {
        use std::os::fd::AsRawFd;
        if unsafe { libc::ioctl(destination.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } == 0 {
            return Ok(());
        }
    }
    let length = source.metadata()?.len();
    destination.set_len(length)?;
    let mut buffer = vec![0; IMAGE_CHUNK_SIZE];
    #[cfg(unix)]
    if native && copy_allocated_ranges(&mut source, &mut destination, length, &mut buffer)? {
        return Ok(());
    }
    source.seek(SeekFrom::Start(0))?;
    destination.seek(SeekFrom::Start(0))?;
    copy_zero_sparse(&mut source, &mut destination, length, &mut buffer)
}

fn copy_zero_sparse(
    source: &mut File,
    destination: &mut File,
    length: u64,
    buffer: &mut [u8],
) -> io::Result<()> {
    let mut remaining = length;
    while remaining != 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..count])?;
        for block in buffer[..count].chunks(4096) {
            if block.iter().all(|&byte| byte == 0) {
                destination.seek(SeekFrom::Current(block.len() as i64))?;
            } else {
                destination.write_all(block)?;
            }
        }
        remaining -= count as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn mark_sparse(file: &File) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "DeviceIoControl"]
        fn device_io_control(
            device: *mut c_void,
            control: u32,
            input: *mut c_void,
            input_size: u32,
            output: *mut c_void,
            output_size: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
    const FSCTL_SET_SPARSE: u32 = (9 << 16) | (49 << 2);
    let mut returned = 0;
    let result = unsafe {
        device_io_control(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn copy_allocated_ranges(
    source: &mut File,
    destination: &mut File,
    length: u64,
    buffer: &mut [u8],
) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut offset = 0;
    while offset < length {
        let start =
            unsafe { libc::lseek(source.as_raw_fd(), offset as libc::off_t, libc::SEEK_DATA) };
        if start < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENXIO) => Ok(true),
                Some(libc::EINVAL | libc::ENOTSUP) => Ok(false),
                _ => Err(error),
            };
        }
        let end = unsafe { libc::lseek(source.as_raw_fd(), start, libc::SEEK_HOLE) };
        if end < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EINVAL | libc::ENOTSUP) => Ok(false),
                _ => Err(error),
            };
        }
        let end = (end as u64).min(length);
        if end <= start as u64 {
            return Ok(false);
        }
        source.seek(SeekFrom::Start(start as u64))?;
        destination.seek(SeekFrom::Start(start as u64))?;
        copy_zero_sparse(source, destination, end - start as u64, buffer)?;
        offset = end;
    }
    Ok(true)
}

#[derive(Debug)]
pub struct ImageSnapshot {
    path: PathBuf,
    file: std::cell::RefCell<File>,
    length: usize,
}

pub fn snapshot_image(path: impl AsRef<Path>) -> io::Result<ImageSnapshot> {
    let destination = PathBuf::from(super::temp_image_path("image-snapshot"));
    if let Err(error) = copy_sparse(path, &destination) {
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    let opened = (|| {
        let metadata = destination.metadata()?;
        let length = usize::try_from(metadata.len()).map_err(io::Error::other)?;
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o600);
        }
        // Windows has only the read-only attribute; clearing it is exactly
        // what is wanted there. The lint is about Unix, where it would make
        // the file world-writable, and that branch uses the mode above.
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&destination, permissions)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)?;
        Ok::<_, io::Error>((file, length))
    })();
    match opened {
        Ok((file, length)) => Ok(ImageSnapshot {
            path: destination,
            file: std::cell::RefCell::new(file),
            length,
        }),
        Err(error) => {
            let _ = std::fs::remove_file(destination);
            Err(error)
        }
    }
}

impl ImageSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn read(&self, range: Range<usize>) -> Vec<u8> {
        let mut bytes = vec![0; range.len()];
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file
                .borrow()
                .read_exact_at(&mut bytes, range.start as u64)
                .expect("read image range");
        }
        #[cfg(not(unix))]
        {
            let mut file = self.file.borrow_mut();
            file.seek(SeekFrom::Start(range.start as u64))
                .expect("seek image range");
            file.read_exact(&mut bytes).expect("read image range");
        }
        bytes
    }

    pub fn write(&mut self, offset: usize, bytes: &[u8]) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file
                .get_mut()
                .write_all_at(bytes, offset as u64)
                .expect("write image range");
        }
        #[cfg(not(unix))]
        {
            let file = self.file.get_mut();
            file.seek(SeekFrom::Start(offset as u64))
                .expect("seek image range");
            file.write_all(bytes).expect("write image range");
        }
        if !bytes.is_empty() {
            self.length = self.length.max(offset + bytes.len());
        }
    }

    pub fn xor(&mut self, offset: usize, mask: u8) {
        let byte = self.read(offset..offset + 1)[0] ^ mask;
        self.write(offset, &[byte]);
    }

    pub fn truncate(&mut self, length: usize) {
        self.file
            .get_mut()
            .set_len(length as u64)
            .expect("resize snapshot");
        self.length = length;
    }

    pub fn copy_to(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if path.exists() {
            let mut source = File::open(&self.path).expect("open snapshot");
            let mut destination = File::create(path).expect("open destination");
            #[cfg(windows)]
            mark_sparse(&destination).expect("mark destination sparse");
            destination
                .set_len(self.len() as u64)
                .expect("size destination");
            copy_zero_sparse(
                &mut source,
                &mut destination,
                self.len() as u64,
                &mut vec![0; IMAGE_CHUNK_SIZE],
            )
            .expect("copy snapshot");
        } else {
            copy_sparse(&self.path, path).expect("copy snapshot");
        }
    }

    pub fn digest(&self) -> [u8; 32] {
        image_digest(&self.path).expect("digest snapshot")
    }

    pub fn matches(&self, path: impl AsRef<Path>) -> bool {
        match first_image_difference(&self.path, path).expect("compare image") {
            None => true,
            Some(difference) => {
                eprintln!("images differ: {difference}");
                false
            }
        }
    }
}

impl Clone for ImageSnapshot {
    fn clone(&self) -> Self {
        snapshot_image(&self.path).expect("clone image snapshot")
    }
}

impl PartialEq for ImageSnapshot {
    fn eq(&self, other: &Self) -> bool {
        match first_image_difference(&self.path, &other.path).expect("compare snapshots") {
            None => true,
            Some(difference) => {
                eprintln!("images differ: {difference}");
                false
            }
        }
    }
}

impl Drop for ImageSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
