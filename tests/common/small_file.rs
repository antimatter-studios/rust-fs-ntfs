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
