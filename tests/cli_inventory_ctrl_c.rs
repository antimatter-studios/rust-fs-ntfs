#![cfg(windows)]

//! Send a real console Ctrl-C to the installed CLI process. The fixture is
//! authored on Windows and downloaded by CI before the CLI integration tier.

use std::ffi::{c_void, OsStr};
use std::io::Read;
use std::os::windows::{ffi::OsStrExt, process::CommandExt};
use std::process::{Child, Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

const CTRL_C_EVENT: u32 = 0;

#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
    fn FreeConsole() -> i32;
    fn GenerateConsoleCtrlEvent(ctrl_event: u32, process_group_id: u32) -> i32;
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    fn CreateEventW(
        security: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

// Keep this test process alive while attached to the child's private
// console. Cargo and the test host stay attached to their own console.
extern "system" fn keep_test_runner_alive(_: u32) -> i32 {
    1
}

struct ReadyEvent {
    handle: *mut c_void,
    name: String,
}

impl ReadyEvent {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            "Local\\ntfs-inventory-ctrl-c-{}-{unique}",
            std::process::id()
        );
        let wide: Vec<u16> = OsStr::new(&name).encode_wide().chain(Some(0)).collect();
        // SAFETY: no inherited handle or security pointer; wide is NUL terminated.
        let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
        assert!(
            !handle.is_null(),
            "create readiness event: {}",
            std::io::Error::last_os_error()
        );
        Self { handle, name }
    }

    fn wait(&self) {
        // SAFETY: self owns the event until Drop. WAIT_OBJECT_0 is zero.
        assert_eq!(
            unsafe { WaitForSingleObject(self.handle, 10_000) },
            0,
            "CLI did not announce its installed console handler"
        );
    }
}

impl Drop for ReadyEvent {
    fn drop(&mut self) {
        // SAFETY: this is the one close of the handle returned by CreateEventW.
        unsafe { CloseHandle(self.handle) };
    }
}

struct RunningChild(Option<Child>);

impl RunningChild {
    fn id(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }

    fn output(&mut self) -> Output {
        self.0
            .take()
            .unwrap()
            .wait_with_output()
            .expect("wait for inventory CLI")
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct ConsoleAttachment {
    restore_parent: bool,
    attached: bool,
}

impl ConsoleAttachment {
    fn attach(process_id: u32) -> Self {
        // SAFETY: this test binary has only this one test. The child's
        // private console keeps Cargo and its host outside the broadcast.
        let mut guard = Self {
            restore_parent: unsafe { FreeConsole() } != 0,
            attached: false,
        };
        assert_ne!(
            unsafe { AttachConsole(process_id) },
            0,
            "attach to the CLI console: {}",
            std::io::Error::last_os_error()
        );
        guard.attached = true;
        // AttachConsole resets the handler table, so register AFTER attaching.
        // https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler
        assert_ne!(
            unsafe { SetConsoleCtrlHandler(Some(keep_test_runner_alive), 1) },
            0
        );
        guard
    }

    fn restore(&mut self) {
        assert_ne!(unsafe { FreeConsole() }, 0, "detach from the CLI console");
        self.attached = false;
        if self.restore_parent {
            assert_ne!(
                unsafe { AttachConsole(ATTACH_PARENT_PROCESS) },
                0,
                "restore the test console"
            );
            self.restore_parent = false;
        }
    }
}

impl Drop for ConsoleAttachment {
    fn drop(&mut self) {
        // Best-effort restoration also runs if an assertion unwinds.
        unsafe {
            if self.attached {
                FreeConsole();
            }
            if self.restore_parent {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

fn image_digest(image: &str) -> [u8; 32] {
    let mut file = std::fs::File::open(image).expect("open Windows image for hash");
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).expect("hash Windows image");
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    hash.finalize().into()
}

#[test]
fn ctrl_c_to_inventory_child_prints_a_resumable_batch_line() {
    const {
        assert!(
            cfg!(feature = "harness"),
            "run the console test with --features cli,harness"
        );
    }
    let image = "test-disks/ntfs-inventory.img";
    assert!(
        std::path::Path::new(image).exists(),
        "missing Windows fixture: {image}"
    );
    let before = image_digest(image);

    let ready = ReadyEvent::new();
    let mut child = RunningChild(Some(
        Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
            .args(["fs.ntfs", image, "inventory", "--max-slots", "1000000000"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("FS_NTFS_TEST_CONSOLE_READY", &ready.name)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .expect("spawn actual inventory CLI"),
    ));

    ready.wait();
    let mut console = ConsoleAttachment::attach(child.id());
    // SAFETY: both processes are attached only to the child's private console.
    assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
    let output = child.output();
    console.restore();
    assert!(
        output.status.success(),
        "CLI exited {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let batch: serde_json::Value = stdout
        .lines()
        .last()
        .expect("CLI printed a batch line")
        .parse()
        .expect("batch line is JSON");
    assert_eq!(batch["type"], "batch", "{stdout}");
    assert_eq!(batch["interrupted"], true, "{stdout}");
    assert_eq!(batch["finished"], false, "{stdout}");
    assert!(
        batch["next_cursor"].as_str().is_some(),
        "interrupted batch must be resumable: {stdout}"
    );

    assert_eq!(
        batch["next_cursor"], "0",
        "cancellation precedes the first read: {stdout}"
    );

    let resumed = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .args([
            "fs.ntfs",
            image,
            "inventory",
            "--max-slots",
            "1000000000",
            "--cursor",
            "0",
        ])
        .output()
        .expect("resume the cancelled batch");
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let lines: Vec<serde_json::Value> = String::from_utf8(resumed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = lines.last().expect("resumed batch line");
    assert_eq!(last["interrupted"], false);
    assert_eq!(last["finished"], true);
    let references: Vec<u64> = lines
        .iter()
        .filter(|line| line["type"] == "record")
        .map(|line| line["file_reference"].as_str().unwrap().parse().unwrap())
        .collect();
    let windows_ids = std::fs::read_to_string("test-disks/ntfs-inventory.fileids.txt")
        .expect("Windows fsutil references");
    assert_eq!(
        windows_ids.lines().count(),
        9,
        "eight files and one directory must be graded"
    );
    for line in windows_ids.lines() {
        let hex = line
            .rsplit("0x")
            .next()
            .expect("fsutil hexadecimal file ID")
            .trim();
        let reference = u128::from_str_radix(hex, 16).expect("parse Windows file ID");
        assert_eq!(reference >> 64, 0, "{line}");
        assert!(
            references.contains(&(reference as u64)),
            "resumed CLI lost Windows file reference: {line}"
        );
    }
    assert_eq!(
        image_digest(image),
        before,
        "cancellation and resume changed the source image"
    );
}
