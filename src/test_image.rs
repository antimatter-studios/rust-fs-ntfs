#![allow(dead_code)]

use crate::test_scratch::temp_image_path;

#[path = "../tests/common/small_file.rs"]
mod small_file;
use small_file::IMAGE_CHUNK_SIZE;

#[path = "../tests/common/image.rs"]
mod image;
pub use image::{assert_images_equal, snapshot_image};
