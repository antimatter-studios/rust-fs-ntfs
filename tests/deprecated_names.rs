//! The names the driver's own traits had before rust-fs-core's shared
//! layers (#1385) still build and answer for one release, deprecated.
#![allow(deprecated)]

use fs_core::BlockRead;
use fs_ntfs::primitives::PrimitiveAPI;
use fs_ntfs::volume::{RawVolume, VolumeAPI};
use std::sync::Arc;

#[derive(Clone)]
struct Image(Arc<Vec<u8>>);

impl BlockRead for Image {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        output.copy_from_slice(&self.0[offset as usize..offset as usize + output.len()]);
        Ok(())
    }
    fn size_bytes(&self) -> u64 {
        self.0.len() as u64
    }
}

#[test]
fn the_old_trait_names_still_answer() {
    let raw = RawVolume::open(Image(Arc::new(vec![0; 8192])));
    assert_eq!(raw.size_bytes(), 8192);
    assert!(!raw.is_writable());
    let record = raw.read_file_record_at(1024, 1024, 512).unwrap();
    assert!(record.fixed.is_none(), "zeros are not a file record");
}
