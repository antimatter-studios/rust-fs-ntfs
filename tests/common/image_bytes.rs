use super::{image_digest, snapshot_image, ImageSnapshot};
use sha2::{Digest, Sha256};
use std::ops::Range;
use std::path::Path;

#[derive(Clone, Debug)]
pub enum ImageBytes {
    Image(ImageSnapshot),
    Small(Vec<u8>),
}

impl ImageBytes {
    pub fn snapshot(path: impl AsRef<Path>) -> std::io::Result<Self> {
        snapshot_image(path).map(Self::Image)
    }

    pub fn new() -> Self {
        Self::Small(Vec::new())
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Image(image) => image.len(),
            Self::Small(bytes) => bytes.len(),
        }
    }

    pub fn get(&self, range: Range<usize>) -> Option<Vec<u8>> {
        if range.start > range.end || range.end > self.len() {
            return None;
        }
        Some(match self {
            Self::Image(image) => image.read(range),
            Self::Small(bytes) => bytes[range].to_vec(),
        })
    }

    pub fn read(&self, range: Range<usize>) -> Vec<u8> {
        self.get(range).expect("image range is within the file")
    }

    pub fn write(&mut self, range: Range<usize>, value: &[u8]) -> Option<()> {
        if range.start > range.end || range.end > self.len() || range.len() != value.len() {
            return None;
        }
        match self {
            Self::Image(image) => image.write(range.start, value),
            Self::Small(bytes) => bytes[range].copy_from_slice(value),
        }
        Some(())
    }

    pub fn xor(&mut self, offset: usize, mask: u8) {
        let byte = self.read(offset..offset + 1)[0] ^ mask;
        self.write(offset..offset + 1, &[byte]).unwrap();
    }

    pub fn truncate(&mut self, length: usize) {
        match self {
            Self::Image(image) => image.truncate(length.min(image.len())),
            Self::Small(bytes) => bytes.truncate(length),
        }
    }

    pub fn resize(&mut self, length: usize, value: u8) {
        assert_eq!(value, 0);
        match self {
            Self::Image(image) => image.truncate(length),
            Self::Small(bytes) => bytes.resize(length, value),
        }
    }

    pub fn small(&self) -> &[u8] {
        match self {
            Self::Small(bytes) => bytes,
            Self::Image(_) => panic!("a disk image must never be materialized in memory"),
        }
    }

    pub fn copy_to(&self, path: impl AsRef<Path>) {
        match self {
            Self::Image(image) => image.copy_to(path),
            Self::Small(bytes) => std::fs::write(path, bytes).expect("write small test data"),
        }
    }
}

impl From<Vec<u8>> for ImageBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Small(bytes)
    }
}

impl PartialEq for ImageBytes {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Image(left), Self::Image(right)) => left == right,
            (Self::Small(left), Self::Small(right)) => left == right,
            _ => panic!("image and small-buffer test storage must not be mixed"),
        }
    }
}

impl PartialEq<Vec<u8>> for ImageBytes {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.small() == other
    }
}

pub trait TestDigest {
    fn test_digest(&self) -> [u8; 32];
}

impl TestDigest for ImageBytes {
    fn test_digest(&self) -> [u8; 32] {
        match self {
            Self::Image(image) => image_digest(image.path()).expect("digest image"),
            Self::Small(bytes) => bytes.test_digest(),
        }
    }
}

impl TestDigest for [u8] {
    fn test_digest(&self) -> [u8; 32] {
        Sha256::digest(self).into()
    }
}

impl TestDigest for Vec<u8> {
    fn test_digest(&self) -> [u8; 32] {
        self.as_slice().test_digest()
    }
}

impl fs_ntfs::block_io::BlockIo for ImageBytes {
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(buffer.len());
        let bytes = self.get(start..end).ok_or_else(|| {
            format!(
                "read of {} bytes at {offset} runs past the end of a {}-byte volume",
                buffer.len(),
                self.len()
            )
        })?;
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_all_at(&mut self, _offset: u64, _buffer: &[u8]) -> Result<(), String> {
        Err("this volume is read-only".to_string())
    }

    fn size(&self) -> u64 {
        self.len() as u64
    }
}
