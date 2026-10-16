//! Harness-only rendezvous for the real Windows console-control test.
//! Release CLI builds do not compile this module or recognize its environment.

use fs_core::cli::CliError;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[link(name = "kernel32")]
extern "system" {
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn SetEvent(event: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

pub(super) fn wait_for_cancel(cancelled: &AtomicBool) -> Result<(), CliError> {
    let Some(name) = std::env::var_os("FS_NTFS_TEST_CONSOLE_READY") else {
        return Ok(());
    };
    let wide: Vec<u16> = name.encode_wide().chain(Some(0)).collect();
    // EVENT_MODIFY_STATE, with no handle inheritance. The test owns the
    // named event; announcing readiness proves the handler is installed.
    // https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-openeventw
    let event = unsafe { OpenEventW(0x0002, 0, wide.as_ptr()) };
    if event.is_null() {
        return Err(CliError::failed(format!(
            "open console-test event: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: OpenEventW returned a live handle, closed exactly once below.
    let signalled = unsafe { SetEvent(event) };
    let error = std::io::Error::last_os_error();
    unsafe { CloseHandle(event) };
    if signalled == 0 {
        return Err(CliError::failed(format!(
            "signal console-test readiness: {error}"
        )));
    }

    // A bound cannot lengthen a small MFT scan. Keep the harness process
    // alive until its real console handler observes the event, rather than
    // guessing how long startup or scanning will take on the CI runner.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cancelled.load(Ordering::Relaxed) {
        if Instant::now() >= deadline {
            return Err(CliError::failed("console-test Ctrl-C was not received"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
