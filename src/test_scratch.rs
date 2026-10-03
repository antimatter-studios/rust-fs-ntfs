//! The one place a test allocates a generated image.
//!
//! Unit tests in `src/` compile this as `crate::test_scratch`, and the
//! integration tests compile the same file through `tests/common`, so there
//! is one implementation and `tests/temp_image_policy.rs` can refuse every
//! other spelling of a generated image path, including `temp_dir()`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Paths of generated images owned by the current test worker.
///
/// The registry's destructor runs when the worker exits, including after a
/// panicking test. Keeping the guard here rather than in each fixture builder
/// also lets builders continue returning `String` without dropping the guard
/// before their caller uses the image.
struct TempImages(RefCell<Vec<PathBuf>>);

impl Drop for TempImages {
    fn drop(&mut self) {
        for path in self.0.get_mut().drain(..).rev() {
            let _ = std::fs::remove_file(path);
        }
    }
}

thread_local! {
    static TEMP_IMAGES: TempImages = const { TempImages(RefCell::new(Vec::new())) };
}

/// Allocate a collision-safe, panic-cleaned image path under `test-disks/`.
pub fn temp_image_path(stem: impl AsRef<str>) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let stem = stem.as_ref();
    assert!(!stem.is_empty(), "temporary image stem must not be empty");
    assert!(
        stem.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')),
        "temporary image stem contains a path separator or unsupported byte: {stem:?}"
    );

    std::fs::create_dir_all("test-disks").expect("create test-disks directory");
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(format!(
        "test-disks/_{stem}_{}_{}.img",
        std::process::id(),
        serial
    ));
    TEMP_IMAGES.with(|images| images.0.borrow_mut().push(path.clone()));
    path.into_os_string()
        .into_string()
        .expect("temporary image path is UTF-8")
}
