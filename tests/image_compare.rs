mod common;

use common::{
    first_image_difference, image_digest, snapshot_image, write_image_region, IMAGE_CHUNK_SIZE,
};
use sha2::{Digest, Sha256};
use std::fs::File;

fn sparse_image(length: u64) -> String {
    let path = common::temp_image_path("image-compare-test");
    File::create(&path).unwrap().set_len(length).unwrap();
    path
}

#[test]
fn equal_images_include_partial_chunks_and_sparse_holes() {
    let path = sparse_image((64 * IMAGE_CHUNK_SIZE + 17) as u64);
    write_image_region(&path, 7, b"first extent").unwrap();
    write_image_region(&path, (63 * IMAGE_CHUNK_SIZE + 3) as u64, b"last extent").unwrap();
    let snapshot = snapshot_image(&path).unwrap();
    assert_eq!(
        first_image_difference(&path, snapshot.path())
            .unwrap()
            .map(|difference| difference.offset()),
        None
    );
    assert_eq!(image_digest(&path).unwrap(), snapshot.digest());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = snapshot.path().metadata().unwrap();
        assert!(
            metadata.blocks() * 512 < metadata.len() / 2,
            "snapshot materialized sparse holes"
        );
    }
}

#[test]
fn differences_name_the_first_byte_at_start_middle_and_end() {
    let length = 3 * IMAGE_CHUNK_SIZE + 17;
    for offset in [
        0,
        IMAGE_CHUNK_SIZE - 1,
        IMAGE_CHUNK_SIZE,
        IMAGE_CHUNK_SIZE + 23,
        length - 1,
    ] {
        let path = sparse_image(length as u64);
        let snapshot = snapshot_image(&path).unwrap();
        write_image_region(&path, offset as u64, &[1]).unwrap();
        assert_eq!(
            first_image_difference(&path, snapshot.path())
                .unwrap()
                .map(|difference| difference.offset()),
            Some(offset as u64)
        );
        assert_eq!(
            first_image_difference(snapshot.path(), &path)
                .unwrap()
                .map(|difference| difference.offset()),
            Some(offset as u64)
        );
    }
}

#[test]
fn length_differences_name_the_first_missing_byte_in_either_direction() {
    for length in [0, 19, IMAGE_CHUNK_SIZE] {
        let left = sparse_image(length as u64);
        let right = sparse_image((length + 1) as u64);
        assert_eq!(
            first_image_difference(&left, &right)
                .unwrap()
                .map(|difference| difference.offset()),
            Some(length as u64)
        );
        assert_eq!(
            first_image_difference(&right, &left)
                .unwrap()
                .map(|difference| difference.offset()),
            Some(length as u64)
        );
    }
}

#[test]
fn a_length_difference_is_reported_before_comparing_bytes() {
    let left = sparse_image(17);
    let right = sparse_image(19);
    write_image_region(&right, 3, &[1]).unwrap();
    assert_eq!(
        first_image_difference(&left, &right)
            .unwrap()
            .map(|difference| difference.offset()),
        Some(17)
    );
}

#[test]
fn an_empty_snapshot_is_equal_and_removed_on_drop() {
    let image = sparse_image(0);
    let snapshot = snapshot_image(&image).unwrap();
    let path = snapshot.path().to_path_buf();
    common::assert_images_equal(&image, &path, "image contents");
    drop(snapshot);
    assert!(!path.exists());
}

#[test]
fn a_snapshot_and_its_clone_are_independent() {
    let image = sparse_image((2 * IMAGE_CHUNK_SIZE) as u64);
    let snapshot = snapshot_image(&image).unwrap();
    let mut clone = snapshot.clone();
    clone.write(0, &[42]);
    assert_eq!(
        first_image_difference(snapshot.path(), clone.path())
            .unwrap()
            .map(|difference| difference.offset()),
        Some(0)
    );
    common::assert_images_equal(&image, snapshot.path(), "image contents");
}

#[test]
fn digest_matches_the_standard_sha256_vector_and_multi_chunk_input() {
    let image = sparse_image(3);
    write_image_region(&image, 0, b"abc").unwrap();
    assert_eq!(
        image_digest(&image).unwrap().as_slice(),
        Sha256::digest(b"abc").as_slice()
    );
    let image = sparse_image((2 * IMAGE_CHUNK_SIZE + 13) as u64);
    let mut expected = Sha256::new();
    let chunk = vec![0; IMAGE_CHUNK_SIZE];
    expected.update(&chunk);
    expected.update(&chunk);
    expected.update([0; 13]);
    assert_eq!(
        image_digest(&image).unwrap().as_slice(),
        expected.finalize().as_slice()
    );
}

#[test]
fn missing_images_are_errors_not_equal() {
    let missing = common::temp_image_path("image-compare-missing");
    let image = sparse_image(0);
    assert!(first_image_difference(&image, &missing).is_err());
    assert!(image_digest(&missing).is_err());
    assert!(snapshot_image(&missing).is_err());
}

#[test]
fn snapshot_length_tracks_shrinks_extensions_and_positioned_writes() {
    let image = sparse_image(19);
    let mut snapshot = snapshot_image(&image).unwrap();
    snapshot.truncate(7);
    assert_eq!(snapshot.len(), 7);
    snapshot.truncate(23);
    assert_eq!(snapshot.len(), 23);
    assert_eq!(snapshot.read(7..23), vec![0; 16]);
    snapshot.write(27, b"tail");
    assert_eq!(snapshot.len(), 31);
    assert_eq!(snapshot.path().metadata().unwrap().len(), 31);
    assert_eq!(snapshot.read(23..31), b"\0\0\0\0tail");
    snapshot.write(100, b"");
    assert_eq!(snapshot.len(), 31);
    assert_eq!(std::fs::metadata(&image).unwrap().len(), 19);
}

#[test]
fn a_prefix_comparison_checks_bytes_even_when_lengths_differ() {
    let left = sparse_image(17);
    let right = sparse_image(19);
    write_image_region(&right, 3, &[1]).unwrap();
    assert_eq!(
        common::first_image_difference_in_prefix(&left, &right, 17)
            .unwrap()
            .map(|difference| difference.offset()),
        Some(3)
    );
    assert!(common::first_image_difference_in_prefix(&left, &right, 3)
        .unwrap()
        .is_none());
    assert!(common::first_image_difference_in_prefix(&left, &right, 18).is_err());
}

#[test]
fn portable_copy_preserves_holes_and_the_trailing_length() {
    let image = sparse_image((64 * IMAGE_CHUNK_SIZE + 19) as u64);
    write_image_region(&image, 4095, b"cross a block boundary").unwrap();
    write_image_region(&image, (63 * IMAGE_CHUNK_SIZE) as u64, b"another extent").unwrap();
    let copied = common::temp_image_path("portable-sparse-copy");
    common::copy_sparse_portable(&image, &copied).unwrap();
    common::assert_images_equal(&image, &copied, "image contents");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(&copied).unwrap();
        assert!(metadata.blocks() * 512 < metadata.len() / 2);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_ne!(
            std::fs::metadata(&copied).unwrap().file_attributes() & 0x200,
            0
        );
    }
}

#[test]
fn sparse_copy_never_overwrites_an_existing_destination() {
    let image = sparse_image(19);
    let destination = sparse_image(23);
    write_image_region(&destination, 0, b"existing").unwrap();
    let digest = image_digest(&destination).unwrap();
    assert!(common::copy_sparse(&image, &destination).is_err());
    assert!(common::copy_sparse_portable(&image, &destination).is_err());
    assert_eq!(image_digest(&destination).unwrap(), digest);
}

#[test]
fn small_file_reads_are_bounded_below_one_mib() {
    let small = sparse_image((IMAGE_CHUNK_SIZE - 1) as u64);
    assert_eq!(
        common::read_small_file(&small).unwrap().len(),
        IMAGE_CHUNK_SIZE - 1
    );
    let large = sparse_image((3 * IMAGE_CHUNK_SIZE) as u64);
    assert!(common::read_small_file(&large).is_err());
}

#[test]
fn a_read_only_source_produces_a_writable_independent_snapshot() {
    let image = sparse_image(19);
    let mut permissions = std::fs::metadata(&image).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&image, permissions).unwrap();
    let mut snapshot = snapshot_image(&image).unwrap();
    snapshot.write(7, &[1]);
    assert_eq!(
        first_image_difference(&image, snapshot.path())
            .unwrap()
            .map(|difference| difference.offset()),
        Some(7)
    );
    #[cfg(windows)]
    {
        let mut permissions = std::fs::metadata(&image).unwrap().permissions();
        // Windows: clear the read-only attribute so the image can be deleted.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&image, permissions).unwrap();
    }
}

#[test]
#[should_panic(expected = "first differing byte at 0x7")]
fn assertion_reports_the_differing_offset() {
    let left = sparse_image(19);
    let right = sparse_image(19);
    write_image_region(&right, 7, &[1]).unwrap();
    common::assert_images_equal(&left, &right, "image contents");
}
