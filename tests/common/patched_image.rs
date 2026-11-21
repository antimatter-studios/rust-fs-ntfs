//! A disk image read from its file a span at a time, with changes laid over
//! it in memory: what a test needs to damage a Windows-made image without
//! ever holding it whole, and to hand it to the driver as a device.

use fs_core::{BlockDevice, BlockRead, FileDevice};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct PatchedImage {
    file: Arc<FileDevice>,
    patches: Mutex<Vec<(u64, Vec<u8>)>>,
}

impl Clone for PatchedImage {
    /// A copy with the same changes, which then changes on its own.
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            patches: Mutex::new(self.patches.lock().unwrap().clone()),
        }
    }
}

impl PatchedImage {
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = FileDevice::open(path.as_ref()).map_err(std::io::Error::other)?;
        Ok(Self {
            file: Arc::new(file),
            patches: Mutex::new(Vec::new()),
        })
    }

    /// `length` bytes from `offset`, changes included.
    pub fn read(&self, offset: u64, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        BlockRead::read_at(self, offset, &mut bytes).expect("read the image");
        bytes
    }

    pub fn write(&mut self, offset: u64, bytes: &[u8]) {
        self.patches.lock().unwrap().push((offset, bytes.to_vec()));
    }

    pub fn fill(&mut self, offset: u64, length: usize, byte: u8) {
        self.write(offset, &vec![byte; length]);
    }

    /// Replaces the byte at `offset` with what `change` makes of it.
    pub fn update(&mut self, offset: u64, change: impl FnOnce(u8) -> u8) {
        let byte = change(self.read(offset, 1)[0]);
        self.write(offset, &[byte]);
    }

    /// The SHA-256 of the whole image as it reads now, a chunk at a time.
    pub fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        let size = self.size_bytes();
        let mut chunk = vec![0; super::IMAGE_CHUNK_SIZE];
        let mut at = 0;
        while at < size {
            let count = (size - at).min(chunk.len() as u64) as usize;
            BlockRead::read_at(self, at, &mut chunk[..count]).expect("read the image");
            digest.update(&chunk[..count]);
            at += count as u64;
        }
        digest.finalize().into()
    }
}

impl BlockRead for PatchedImage {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.file.read_at(offset, output)?;
        let end = offset + output.len() as u64;
        for (at, bytes) in self.patches.lock().unwrap().iter() {
            let from = (*at).max(offset);
            let to = (at + bytes.len() as u64).min(end);
            if from < to {
                output[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[(from - at) as usize..(to - at) as usize]);
            }
        }
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.file.size_bytes()
    }
}

/// Writes go to the changes, never to the file.
impl BlockDevice for PatchedImage {
    fn write_at(&self, offset: u64, input: &[u8]) -> fs_core::Result<()> {
        if offset
            .checked_add(input.len() as u64)
            .is_none_or(|end| end > self.size_bytes())
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: input.len() as u64,
                size: self.size_bytes(),
            });
        }
        self.patches.lock().unwrap().push((offset, input.to_vec()));
        Ok(())
    }
    fn is_writable(&self) -> bool {
        true
    }
}
