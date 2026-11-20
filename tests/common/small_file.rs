use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub const IMAGE_CHUNK_SIZE: usize = 1024 * 1024;

pub fn read_small_file(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(IMAGE_CHUNK_SIZE as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() == IMAGE_CHUNK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "small test file must be smaller than 1 MiB",
        ));
    }
    Ok(bytes)
}

/// The largest repair plan or journal a test reads whole. These are not
/// disk images: they hold the bytes of the metadata blocks one repair
/// writes, a few MiB for the largest Windows capture, and their decoders
/// take a slice.
pub const MAX_REPAIR_FILE_BYTES: usize = 64 * 1024 * 1024;

/// Reads a repair plan or journal whole, refusing one larger than
/// [`MAX_REPAIR_FILE_BYTES`] rather than reading a disk image by mistake.
pub fn read_repair_file(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_REPAIR_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_REPAIR_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a repair plan or journal must be smaller than 64 MiB",
        ));
    }
    Ok(bytes)
}
