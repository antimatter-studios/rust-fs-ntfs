//! THE KERNEL ORACLE (#486): Linux's own ntfs3 driver, a third NTFS
//! implementation, reading what this crate wrote and writing what this
//! crate must read.
//!
//! Windows' chkdsk grades the matrix and ntfs-3g reads in the CLI oracle
//! tier, but neither is a mount by the kernel. ntfs3 is independent of
//! both, and it complains in the kernel log about structures it finds
//! inconsistent, so its log is a verdict too.
//!
//! IT RUNS ONLY IN THE fs-linux-test-harness VM, never on the host: a
//! mount needs root and a kernel with ntfs3, which the guest gets from
//! `scripts/vm-setup.sh`. The guest sees this repository at `/repo`, so an
//! image a test writes under `test-disks/` crosses as a path, not a copy.
//!
//! ONE GUEST CALL PER MOUNT. The guest script copies the image to its own
//! disk, mounts it, runs what it was given, unmounts, and copies the image
//! back for a writable mount. A loop mount pointed at a file on the 9p
//! share would mix the guest's page cache with the host's view of one
//! file, and the copy costs a fraction of a second for these images.
//!
//! NOTHING SKIPS. A missing harness, a VM that will not boot, or a guest
//! without ntfs3 fails the test that needed it, naming the task that
//! fixes it.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

/// This repository: cargo runs an integration test from the package root.
fn repo() -> &'static Path {
    static REPO: OnceLock<PathBuf> = OnceLock::new();
    REPO.get_or_init(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// The harness's driver script, or a panic naming `chore siblings`.
fn vm_script() -> PathBuf {
    let path = repo().join("../fs-linux-test-harness/scripts/vm.sh");
    assert!(
        path.is_file(),
        "the fs-linux-test-harness sibling is not checked out at {}. `chore siblings` \
         puts it there at the ref chores.yml pins. ntfs3 runs in its VM and nowhere \
         else, so there is nothing to fall back to.",
        path.display()
    );
    path
}

/// True inside the harness guest, where every command has `FLTH_GUEST=1`.
fn in_guest() -> bool {
    std::env::var_os("FLTH_GUEST").is_some_and(|value| value == "1")
}

/// Run a harness command from the repository root, where the harness finds
/// `fs-linux-test-harness.toml`.
fn vm(args: &[&str]) -> Output {
    Command::new(vm_script())
        .args(args)
        .current_dir(repo())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", vm_script().display()))
}

/// Bring the VM up once per test process. `vm.sh up` returns at once when
/// it is already running, which is the normal case under `chore
/// test:kernel`; a run by hand boots it.
fn session() {
    static SESSION: OnceLock<()> = OnceLock::new();
    SESSION.get_or_init(|| {
        if in_guest() {
            return;
        }
        let out = vm(&["up"]);
        assert!(
            out.status.success(),
            "the fs-linux-test-harness VM would not start, so ntfs3 cannot mount \
             anything. `chore vm:host:check` says what this host is missing; `chore \
             vm:destroy` clears a broken machine.\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    });
}

/// Run a script in the guest as root, under `bash -euo pipefail`.
fn guest(script: &str) -> Output {
    session();
    let script = format!("set -euo pipefail\n{script}");
    if in_guest() {
        return Command::new("bash")
            .arg("-c")
            .arg(&script)
            .current_dir(repo())
            .stdin(Stdio::null())
            .output()
            .expect("run bash in the guest");
    }
    vm(&["exec", &script])
}

/// One argument, as the guest's shell reads it.
fn quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', r"'\''"))
}

/// Where the guest sees `image`, which must be inside this repository.
fn guest_path(image: &str) -> String {
    let host = repo().join(image);
    let relative = host.strip_prefix(repo()).unwrap_or_else(|_| {
        panic!(
            "the kernel oracle was given {image}, outside {}: the guest sees this \
             repository and nothing else of the host",
            repo().display()
        )
    });
    if in_guest() {
        return host.to_string_lossy().into_owned();
    }
    format!("/repo/{}", relative.to_string_lossy())
}

/// Mount `image` with ntfs3, run `script` in it, unmount, and for a
/// writable mount bring the image back with what the kernel wrote.
///
/// In `script`, `$MNT` is the mount point. Its stdout comes back as the
/// result. ntfs3's kernel log lines from the mount through the unmount are
/// printed as `dmesg<TAB><TAB><line>`. The mount is refused if it fails,
/// with everything the guest said.
///
/// ONE MOUNT AT A TIME. The kernel log is one stream for the whole guest:
/// with mounts running side by side, a window from one mount's marker to its
/// unmount holds the others' lines too, and one test would fail for what
/// another volume did.
fn mounted(image: &str, writable: bool, script: &str) -> String {
    static ONE_MOUNT: Mutex<()> = Mutex::new(());
    let _one = ONE_MOUNT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (options, losetup) = if writable {
        ("rw", "")
    } else {
        ("ro", "--read-only")
    };
    let copy_back = if writable {
        format!(
            "cp --sparse=always \"$work/image\" {}",
            quote(&guest_path(image))
        )
    } else {
        String::new()
    };
    let out = guest(&format!(
        r#"work="$(mktemp -d /var/tmp/ntfs3-oracle.XXXXXX)"
mnt="$work/mnt"
mkdir -p "$mnt"
cp --sparse=always {image} "$work/image"
modprobe ntfs3
marker="kernel-oracle-mount-$(basename "$work")"
echo "$marker" > /dev/kmsg
loop="$(losetup --find --show {losetup} "$work/image")"
cleanup() {{
    mountpoint -q "$mnt" && umount "$mnt" || true
    losetup -d "$loop" 2>/dev/null || true
    rm -rf "$work"
}}
trap cleanup EXIT
mount -t ntfs3 -o {options} "$loop" "$mnt"
(cd "$mnt" && MNT="$mnt" bash -euo pipefail -c {script})
umount "$mnt"
losetup -d "$loop"
{copy_back}
dmesg | sed -n "/$marker/,\$p" | sed 1d | grep -i ntfs3 | sed 's/^\[[^]]*\] *//; s/^/dmesg\t\t/' || true"#,
        image = quote(&guest_path(image)),
        script = quote(script),
    ));
    assert!(
        out.status.success(),
        "ntfs3 could not mount {image} -o {options} in the guest ({:?}):\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Every path in the mounted tree, as `kind<TAB>path<TAB>value`: its type,
/// and for a file its size and SHA-256, for a symlink its target.
///
/// A path the kernel cannot read is reported as `error<TAB>path<TAB>what`
/// rather than ending the walk: what ntfs3 refuses is a finding, so it goes
/// into the comparison like any other difference.
const REPORT: &str = r#"{ find . -mindepth 1 -printf '%P\n' 2>/dev/null || true; } | LC_ALL=C sort | while IFS= read -r path; do
    if ! kind="$(stat -c '%F' "$path" 2>/dev/null)"; then
        printf 'error\t%s\tstat\n' "$path"
        continue
    fi
    kind="$(printf %s "$kind" | tr ' ' '-')"
    printf 'type\t%s\t%s\n' "$path" "$kind"
    case "$kind" in
        regular-file | regular-empty-file)
            printf 'size\t%s\t%s\n' "$path" "$(stat -c '%s' "$path")"
            if sum="$(sha256sum "$path" 2>/dev/null)"; then
                printf 'sha256\t%s\t%s\n' "$path" "${sum%% *}"
            else
                printf 'error\t%s\tread\n' "$path"
            fi
            ;;
        symbolic-link)
            printf 'target\t%s\t%s\n' "$path" "$(readlink "$path")"
            ;;
    esac
done"#;

/// `kind<TAB>path<TAB>value` lines, keyed by `(kind, path)`. Kernel log
/// lines all have the key `("dmesg", "")`, so they are kept apart.
pub fn parse(report: &str) -> (BTreeMap<(String, String), String>, Vec<String>) {
    let mut entries = BTreeMap::new();
    let mut dmesg = Vec::new();
    for line in report.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let kind = fields.next().unwrap_or_default().to_string();
        let path = fields.next().unwrap_or_default().to_string();
        let value = fields.next().unwrap_or_default().to_string();
        if kind == "dmesg" {
            dmesg.push(value);
        } else {
            entries.insert((kind, path), value);
        }
    }
    (entries, dmesg)
}

/// What ntfs3 sees in `image`, mounted read-only: the report above, and
/// everything ntfs3 logged while it was mounted.
pub fn kernel_report(image: &str) -> (BTreeMap<(String, String), String>, Vec<String>) {
    parse(&mounted(image, false, REPORT))
}

/// Mount `image` read-write with ntfs3, run `script` in it, and return the
/// report of the tree it left, read in the same mount. The image comes
/// back with whatever the kernel wrote.
pub fn kernel_write(
    image: &str,
    script: &str,
) -> (BTreeMap<(String, String), String>, Vec<String>) {
    parse(&mounted(
        image,
        true,
        &format!("{script}\ncd \"$MNT\"\n{REPORT}"),
    ))
}

/// Mount `image` with ntfs3, read-only or writable, run `script` in it and
/// return its stdout with the kernel log lines `mounted` appends.
pub fn kernel_run(image: &str, writable: bool, script: &str) -> String {
    mounted(image, writable, script)
}

/// The kernel the guest runs, to name in a failure.
pub fn kernel_release() -> String {
    let out = guest("uname -r");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
