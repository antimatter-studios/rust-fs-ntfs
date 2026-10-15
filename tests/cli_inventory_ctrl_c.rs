#![cfg(windows)]

//! Send a real console Ctrl-C to the installed CLI process. The fixture is
//! authored on Windows and downloaded by CI before the CLI integration tier.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const CTRL_C_EVENT: u32 = 0;

#[link(name = "kernel32")]
extern "system" {
    fn AttachConsole(process_id: u32) -> i32;
    fn FreeConsole() -> i32;
    fn GenerateConsoleCtrlEvent(ctrl_event: u32, process_group_id: u32) -> i32;
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;

// Keep this test process alive while attached to the child's private
// console. Cargo and the test host stay attached to their own console.
extern "system" fn keep_test_runner_alive(_: u32) -> i32 {
    1
}

#[test]
fn ctrl_c_to_inventory_child_prints_a_resumable_batch_line() {
    let image = "test-disks/ntfs-inventory.img";
    assert!(
        std::path::Path::new(image).exists(),
        "missing Windows fixture: {image}"
    );

    // SAFETY: handler is static and only handles Ctrl-C for the duration of
    // this test. The child remains responsible for its own control handler.
    assert_ne!(
        unsafe { SetConsoleCtrlHandler(Some(keep_test_runner_alive), 1) },
        0
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_rust-fs-ntfs"))
        .args(["fs.ntfs", image, "inventory", "--max-slots", "1000000000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("spawn actual inventory CLI");

    // Let the CLI open the image and register its console handler before
    // sending Ctrl-C to its new console. The large bound keeps its scan in
    // progress. Detaching first keeps the Cargo/test-host console untouched.
    thread::sleep(Duration::from_millis(20));
    // SAFETY: attach only to the console just created for this child, send
    // Ctrl-C to its attached processes, then restore the parent console.
    assert_ne!(unsafe { FreeConsole() }, 0, "detach from the test console");
    assert_ne!(
        unsafe { AttachConsole(child.id()) },
        0,
        "attach to the CLI console"
    );
    assert_ne!(unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) }, 0);
    assert_ne!(unsafe { FreeConsole() }, 0, "detach from the CLI console");
    assert_ne!(
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) },
        0,
        "restore the test console"
    );
    let output = child.wait_with_output().expect("wait for inventory CLI");
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

    // SAFETY: remove the temporary process-wide handler after the event.
    assert_ne!(
        unsafe { SetConsoleCtrlHandler(Some(keep_test_runner_alive), 0) },
        0
    );
}
