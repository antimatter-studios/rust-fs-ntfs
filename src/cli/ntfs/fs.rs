//! `fs.ntfs <target> <verb>`: an errand inside an NTFS image or device,
//! without mounting it.
//!
//! The verbs are the shared set: `ls`, `read`, `write`, `mkdir`,
//! `get`/`info`, `set`, `resize`. Metadata is JSON (or `--text`); file
//! content is raw bytes. A verb the library cannot do yet still exists and
//! answers `not implemented` with exit status 3, so a script moved between
//! filesystems fails loudly instead of meaning something else.

use std::ffi::OsString;
use std::io::Write;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};

use super::device::{self, Device};
use crate::common::{CliError, Json, Outcome, Tool};
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::read::{self, VolumeInfo};

pub const TOOL: Tool = Tool {
    name: "fs.ntfs",
    verb: "fs",
    section: 1,
    usage_exit: crate::common::output::EXIT_USAGE,
    about: "List, read, write and inspect an NTFS image or device without mounting it",
    command,
    run,
};

/// The canonical keys every `fs.<fs>` answers, in the shared order.
/// Filesystem specifics are nested under `ntfs`.
pub const KEYS: &[&str] = &[
    "fs",
    "label",
    "total_bytes",
    "free_bytes",
    "block_size",
    "dirty",
    "ntfs",
];

/// NTFS reserves MFT records 0..16 for its metafiles (`$MFT`, `$LogFile`,
/// `$Bitmap`, `$Extend`, ...). `ls` leaves them out unless `--all` asks.
const FIRST_USER_RECORD: u64 = 16;

const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

fn command() -> Cmd {
    Cmd::new("fs.ntfs")
        .about("List, read, write and inspect an NTFS image or device without mounting it")
        .long_about(
            "Work inside an NTFS image or device directly: no mount, no kernel driver.\n\n\
             An escape hatch for an errand (get a file out, read the label, check whether \
             it is dirty), not a place to do real filesystem work: for that, mount it.\n\n\
             Metadata is JSON on stdout (--text for people); `read` writes the file's raw \
             bytes. A failure is {\"error\": \"...\", \"code\": N} on stderr, N being the \
             exit status: 1 failed, 2 wrong command line, 3 not implemented.",
        )
        .arg(
            Arg::new("target")
                .value_name("TARGET")
                .help("The image file or device")
                .value_parser(value_parser!(OsString))
                .required(true),
        )
        .arg(
            Arg::new("offset")
                .long("offset")
                .value_name("BYTES")
                .help(
                    "Where the filesystem starts in TARGET, for a partition in a whole-disk image",
                )
                .value_parser(value_parser!(u64))
                .global(true),
        )
        .args(crate::common::format_args().map(|a| a.global(true)))
        .subcommand_required(true)
        .subcommand(
            Cmd::new("ls")
                .about("List a directory: name, type, size, mode, mtime (and a link's target)")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .default_value("/")
                        .value_parser(value_parser!(OsString)),
                )
                .arg(
                    Arg::new("all")
                        .short('a')
                        .long("all")
                        .action(ArgAction::SetTrue)
                        .help("Include the NTFS metafiles ($MFT, $LogFile, ...: records 0 to 15)"),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img ls /Users\n  \
                     fs.ntfs disk.img ls / | jq -r '.[].name'\n  \
                     fs.ntfs disk.img ls --all --text /",
                ),
        )
        .subcommand(
            Cmd::new("read")
                .about("Write a file's bytes to stdout, or to a file with -o")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .arg(
                    Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("FILE")
                        .value_parser(value_parser!(OsString))
                        .help("Write here instead of stdout"),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img read /notes.txt\n  \
                     fs.ntfs disk.img read /logs/app.log | grep -i error\n  \
                     fs.ntfs disk.img read /backup.zip -o backup.zip",
                ),
        )
        .subcommand(
            Cmd::new("write")
                .about("Create or replace a file with the bytes on stdin")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img write /notes.txt < notes.txt\n  \
                     tar cf - ./dir | fs.ntfs disk.img write /backup.tar\n  \
                     fs.ext4 src.img read /f | fs.ntfs dst.img write /f\n\n\
                     The parent directory must exist. An existing file is replaced.",
                ),
        )
        .subcommand(
            Cmd::new("mkdir")
                .about("Create a directory (its parent must exist)")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img mkdir /backup\n  \
                     fs.ntfs disk.img mkdir /backup/2026",
                ),
        )
        .subcommand(key_command(
            "get",
            "Report the filesystem's properties, or one of them",
        ))
        .subcommand(key_command(
            "info",
            "The same as get: every property, or one of them",
        ))
        .subcommand(
            Cmd::new("set")
                .about("Change a property: label, or dirty (true or false)")
                .arg(Arg::new("key").value_name("KEY").required(true))
                .arg(
                    Arg::new("value")
                        .value_name("VALUE")
                        .required(true)
                        .allow_hyphen_values(true),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img set label \"Backup Volume\"\n  \
                     fs.ntfs disk.img set label \"\"            remove the label\n  \
                     fs.ntfs disk.img set dirty false\n\n\
                     A label is at most 32 UTF-16 code units. \
                     `set dirty false` clears the flag without looking at $LogFile: use it \
                     only on a volume known to be consistent. fsck.ntfs -y clears it only \
                     when the log is empty.",
                ),
        )
        .subcommand(
            Cmd::new("resize")
                .about("Grow or shrink the filesystem (not implemented)")
                .arg(Arg::new("size").value_name("SIZE").required(true))
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Do it without asking"),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img resize 20G --force\n\n\
                     Answers `not implemented` (exit 3): the library cannot resize a volume.",
                ),
        )
        .after_help(
            "Examples:\n  fs.ntfs disk.img ls /\n  \
             fs.ntfs disk.img read /notes.txt > notes.txt\n  \
             fs.ntfs disk.img get label --text\n  \
             fs.ntfs --offset 1048576 whole-disk.img info",
        )
}

fn key_command(name: &'static str, about: &'static str) -> Cmd {
    Cmd::new(name)
        .about(about)
        .arg(
            Arg::new("key")
                .value_name("KEY")
                .help(format!("One of: {} (or ntfs.<field>)", KEYS.join(", "))),
        )
        .after_help(format!(
            "Examples:\n  fs.ntfs disk.img {name}\n  \
             fs.ntfs disk.img {name} label --text\n  \
             fs.ntfs disk.img {name} ntfs.serial_number"
        ))
}

fn run(matches: &ArgMatches) -> Result<Outcome, CliError> {
    let target = matches
        .get_one::<OsString>("target")
        .expect("clap requires the target");
    let (verb, sub) = matches.subcommand().expect("clap requires a verb");
    let offset = sub
        .get_one::<u64>("offset")
        .or_else(|| matches.get_one::<u64>("offset"))
        .copied()
        .unwrap_or(0);
    match verb {
        "ls" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            ls(&mut dev, path_arg(sub)?, sub.get_flag("all"))
        }
        "read" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            read(&mut dev, path_arg(sub)?, sub.get_one("output"))
        }
        "get" | "info" => {
            let (mut dev, info) = device::mount(target, offset, false)?;
            get(
                &mut dev,
                &info,
                sub.get_one::<String>("key").map(String::as_str),
            )
        }
        "set" => set(target, offset, sub),
        "write" => write(target, offset, path_arg(sub)?),
        "mkdir" => mkdir(target, offset, path_arg(sub)?),
        "resize" => Err(CliError::not_implemented(
            "resize: this library cannot resize an NTFS volume",
        )),
        other => unreachable!("clap knows no verb {other}"),
    }
}

/// A path argument. The library takes paths as text, so one that is not
/// UTF-8 cannot name anything on an NTFS volume (whose names are UTF-16).
fn path_arg(sub: &ArgMatches) -> Result<&str, CliError> {
    let path = sub
        .get_one::<OsString>("path")
        .expect("clap requires or defaults the path");
    path.to_str().ok_or_else(|| {
        CliError::failed(format!(
            "{}: not UTF-8; NTFS names are Unicode",
            path.to_string_lossy()
        ))
    })
}

fn ntfs_error(path: &str, e: String) -> CliError {
    CliError::failed(format!("{path}: {e}"))
}

/// What an entry is, from its directory flag and its reparse tag.
fn kind(is_dir: bool, reparse_tag: Option<u32>) -> &'static str {
    match reparse_tag {
        Some(REPARSE_TAG_SYMLINK) => "symlink",
        Some(REPARSE_TAG_MOUNT_POINT) => "junction",
        _ if is_dir => "dir",
        _ => "file",
    }
}

fn type_char(kind: &str) -> char {
    match kind {
        "dir" => 'd',
        "symlink" | "junction" => 'l',
        _ => '-',
    }
}

/// The mode the family reports for each kind, as the C ABI's stat does:
/// NTFS keeps no POSIX permissions.
fn mode(kind: &str) -> &'static str {
    match kind {
        "dir" => "0755",
        "symlink" | "junction" => "0777",
        _ => "0644",
    }
}

/// One `ls` entry: the fields every `fs.<fs>` reports, typed the same way
/// everywhere -- name (string), type (string), size (number), mode (octal
/// string), mtime (seconds since the epoch, number), and target (string)
/// for a link -- plus NTFS's own record number and attribute bits.
fn entry(dev: &mut Device, name: &str, record: u64) -> Result<Json, String> {
    let st = read::read_stat(dev, record)?;
    let reparse = read::read_attribute_value_if_present(dev, record, AttrType::ReparsePoint, None)?;
    let tag = reparse
        .as_deref()
        .filter(|r| r.len() >= 4)
        .map(|r| u32::from_le_bytes([r[0], r[1], r[2], r[3]]));
    let kind = kind(st.is_dir, tag);
    let mut fields = vec![
        ("name", Json::from(name)),
        ("type", Json::from(kind)),
        ("size", Json::from(st.size)),
        ("mode", Json::from(mode(kind))),
        ("mtime", Json::from(read::nt_to_unix(st.modified_nt))),
        ("record", Json::from(record)),
        ("attributes", Json::from(st.file_attributes)),
    ];
    if matches!(kind, "symlink" | "junction") {
        fields.push((
            "target",
            Json::from(reparse.as_deref().and_then(fs_ntfs::reparse_link_target)),
        ));
    }
    Ok(Json::object(fields))
}

fn entry_text(e: &Json) -> String {
    let field = |k: &str| e.get(k).map(Json::to_text).unwrap_or_default();
    let mut line = format!(
        "{}{} {:>12} {}",
        type_char(&field("type")),
        field("mode"),
        field("size"),
        field("name")
    );
    if let Some(target) = e.get("target") {
        line.push_str(&format!(" -> {}", target.to_text()));
    }
    line
}

fn ls(dev: &mut Device, path: &str, all: bool) -> Result<Outcome, CliError> {
    let record = read::resolve_path(dev, path).map_err(|e| ntfs_error(path, e))?;
    let st = read::read_stat(dev, record).map_err(|e| ntfs_error(path, e))?;
    let entries = if st.is_dir {
        let mut listed = Vec::new();
        for d in read::read_dir_entries(dev, record).map_err(|e| ntfs_error(path, e))? {
            if !all && d.record_number < FIRST_USER_RECORD {
                continue;
            }
            let full = format!("{}/{}", path.trim_end_matches('/'), d.name);
            listed.push(entry(dev, &d.name, d.record_number).map_err(|e| ntfs_error(&full, e))?);
        }
        listed.sort_by(|a, b| {
            a.get("name")
                .map(Json::to_text)
                .cmp(&b.get("name").map(Json::to_text))
        });
        listed
    } else {
        let name = path.rsplit('/').next().unwrap_or(path);
        vec![entry(dev, name, record).map_err(|e| ntfs_error(path, e))?]
    };
    let text = entries
        .iter()
        .map(entry_text)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Outcome::report(Json::Arr(entries)).with_text(text))
}

/// Stream a regular file's unnamed `$DATA`. Each chunk is read before it is
/// written, so a file whose clusters turn out unreadable part-way stops
/// with status 1 and what came before stays on stdout; everything that can
/// be refused up front (no such path, a directory, a link) is refused
/// before a byte is written. `-o FILE` writes `FILE.partial` and renames
/// it, so FILE is never left half written.
fn read(dev: &mut Device, path: &str, output: Option<&OsString>) -> Result<Outcome, CliError> {
    let record = read::resolve_path(dev, path).map_err(|e| ntfs_error(path, e))?;
    let st = read::read_stat(dev, record).map_err(|e| ntfs_error(path, e))?;
    if st.is_dir {
        return Err(CliError::failed(format!("{path}: is a directory")));
    }
    let reparse = read::read_attribute_value_if_present(dev, record, AttrType::ReparsePoint, None)
        .map_err(|e| ntfs_error(path, e))?;
    if let Some(value) = reparse.as_deref() {
        if let Some(target) = fs_ntfs::reparse_link_target(value) {
            return Err(CliError::failed(format!(
                "{path}: is a link to {target}; read the target instead"
            )));
        }
    }
    const CHUNK: usize = 1 << 20;
    let mut copy = |sink: &mut dyn Write| -> Result<(), CliError> {
        let mut offset = 0u64;
        while offset < st.size {
            let want = CHUNK.min((st.size - offset) as usize);
            let got = read::read_attribute_range(dev, record, AttrType::Data, None, offset, want)
                .map_err(|e| ntfs_error(path, e))?;
            if got.is_empty() {
                return Err(CliError::failed(format!(
                    "{path}: short read at byte {offset} of {}",
                    st.size
                )));
            }
            sink.write_all(&got)
                .map_err(|e| CliError::failed(format!("write: {e}")))?;
            offset += got.len() as u64;
        }
        sink.flush()
            .map_err(|e| CliError::failed(format!("write: {e}")))
    };
    match output {
        None => copy(&mut std::io::stdout().lock())?,
        Some(file) => {
            let dest = std::path::Path::new(file);
            let mut partial = dest.as_os_str().to_owned();
            partial.push(".partial");
            let partial = std::path::PathBuf::from(partial);
            let mut f = std::fs::File::create(&partial)
                .map_err(|e| CliError::failed(format!("create {}: {e}", partial.display())))?;
            if let Err(e) = copy(&mut f) {
                drop(f);
                let _ = std::fs::remove_file(&partial);
                return Err(e);
            }
            std::fs::rename(&partial, dest)
                .map_err(|e| CliError::failed(format!("rename to {}: {e}", dest.display())))?;
        }
    }
    Ok(Outcome::done())
}

/// `set KEY VALUE`. Every key is one value and nothing else; a key that
/// is reported but cannot be written answers exit 3 by name.
fn set(target: &OsString, offset: u64, sub: &ArgMatches) -> Result<Outcome, CliError> {
    let key = sub.get_one::<String>("key").expect("clap requires the key");
    let value = sub
        .get_one::<String>("value")
        .expect("clap requires the value");
    match key.as_str() {
        "dirty" => {
            let want = match value.as_str() {
                "true" => true,
                "false" => false,
                other => {
                    return Err(CliError::usage(format!(
                        "set dirty takes true or false, not {other:?}"
                    )))
                }
            };
            let mut dev = device::open(target, offset, true)?;
            let changed = if want {
                fs_ntfs::fsck::set_dirty_io(&mut dev)
            } else {
                fs_ntfs::fsck::clear_dirty_io(&mut dev)
            }
            .map_err(|e| CliError::failed(format!("set dirty: {e}")))?;
            let report = Json::object([
                ("dirty", Json::from(want)),
                ("changed", Json::from(changed)),
            ]);
            Ok(Outcome::report(report).with_text(want.to_string()))
        }
        "label" => {
            super::format::check_label(value)
                .map_err(|e| CliError::usage(e.replacen("--label", "set label", 1)))?;
            edit(target, offset, "set label", |dev| {
                fs_ntfs::write::set_volume_label_io(dev, value)
            })?;
            let label = if value.is_empty() {
                Json::Null
            } else {
                Json::from(value.as_str())
            };
            Ok(Outcome::report(Json::object([("label", label)])).with_text(value.clone()))
        }
        k if KEYS.contains(&k) || k.starts_with("ntfs.") => Err(CliError::refused(format!(
            "{k} is read-only{}",
            if k == "total_bytes" {
                "; resize changes the size"
            } else {
                ""
            }
        ))),
        other => Err(CliError::usage(format!(
            "no key {other:?}; the settable keys are dirty and label"
        ))),
    }
}

/// Split an absolute path into its parent and its last name, refusing the
/// root and the names that are not names (`.`, `..`).
fn split_path(path: &str) -> Result<(String, &str), CliError> {
    let trimmed = path.trim_end_matches('/');
    let (parent, name) = trimmed.rsplit_once('/').ok_or_else(|| {
        CliError::usage(format!("{path}: give the path from the root, as /{path}"))
    })?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(CliError::usage(format!("{path}: names no file")));
    }
    let parent = if parent.is_empty() { "/" } else { parent };
    Ok((parent.to_string(), name))
}

/// Run `edit` on a writable open of `target`, then sync it. A volume
/// Windows marked dirty is refused before anything is written, as the C
/// ABI's read-write mount refuses it: its `$LogFile` may hold changes
/// this library cannot replay. A fresh-format volume is upgraded first,
/// as Windows does on its first read-write mount.
fn edit<T>(
    target: &OsString,
    offset: u64,
    what: &str,
    edit: impl FnOnce(&mut Device) -> Result<T, String>,
) -> Result<T, CliError> {
    let (mut dev, info) = device::mount(target, offset, true)?;
    if is_dirty(&info) {
        return Err(CliError::failed(format!(
            "{what}: the volume is dirty, and a write over a $LogFile this library cannot \
             replay could lose changes; check it with fsck.ntfs (or chkdsk on Windows) first"
        )));
    }
    fs_ntfs::fsck::upgrade_volume_version_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: upgrade the volume version: {e}")))?;
    let done = edit(&mut dev).map_err(|e| CliError::failed(format!("{what}: {e}")))?;
    fs_ntfs::block_io::BlockIo::sync(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: {e}")))?;
    Ok(done)
}

/// Create or replace a regular file with everything on stdin. The whole
/// input is read before the image is opened, so a failing producer
/// (`false | fs.ntfs img write /f`) leaves the image as it was, and
/// everything that can be refused (a missing parent, a directory at the
/// path) is refused before anything is written.
fn write(target: &OsString, offset: u64, path: &str) -> Result<Outcome, CliError> {
    let (parent, name) = split_path(path)?;
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut data)
        .map_err(|e| CliError::failed(format!("read stdin: {e}")))?;
    let (created, size) = edit(target, offset, path, |dev| {
        let created = match read::resolve_path(dev, path) {
            Ok(record) => {
                if read::read_stat(dev, record)?.is_dir {
                    return Err("is a directory".to_string());
                }
                false
            }
            Err(e) if e.contains("not found") => {
                let parent_record = read::resolve_path(dev, &parent)?;
                if !read::read_stat(dev, parent_record)?.is_dir {
                    return Err(format!("{parent} is not a directory"));
                }
                fs_ntfs::write::create_file_io(dev, &parent, name)?;
                true
            }
            Err(e) => return Err(e),
        };
        let size = match fs_ntfs::write::replace_file_contents_io(dev, path, &data) {
            Ok(size) => size,
            // A file this call created is removed again, so a failed write
            // does not leave an empty file behind. One that existed keeps
            // whatever the failed replacement left of it.
            Err(e) if created => {
                let undone = fs_ntfs::write::unlink_io(dev, path)
                    .and_then(|()| fs_ntfs::block_io::BlockIo::sync(dev));
                return Err(match undone {
                    Ok(()) => format!("{e} (the new file was removed again)"),
                    Err(u) => format!("{e}; and removing the new file failed: {u}"),
                });
            }
            Err(e) => return Err(e),
        };
        Ok((created, size))
    })?;
    let report = Json::object([
        ("path", Json::from(path)),
        ("bytes", Json::from(size)),
        ("created", Json::from(created)),
    ]);
    let text = format!(
        "{} {path} ({size} bytes)",
        if created { "created" } else { "replaced" }
    );
    Ok(Outcome::report(report).with_text(text))
}

/// Create one directory. Its parent must exist, and nothing may be at the
/// path already.
fn mkdir(target: &OsString, offset: u64, path: &str) -> Result<Outcome, CliError> {
    let (parent, name) = split_path(path)?;
    let record = edit(target, offset, path, |dev| {
        if read::resolve_path(dev, path).is_ok() {
            return Err("already exists".to_string());
        }
        fs_ntfs::write::mkdir_io(dev, &parent, name)
    })?;
    let report = Json::object([("path", Json::from(path)), ("record", Json::from(record))]);
    Ok(Outcome::report(report).with_text(format!("created {path}")))
}

/// Whether `$Volume` carries the dirty flag: the volume was not cleanly
/// unmounted, and Windows will check it before trusting it.
pub fn is_dirty(info: &VolumeInfo) -> bool {
    info.flags & read::VOLUME_IS_DIRTY != 0
}

/// The envelope: the shared keys first, NTFS's own under `ntfs`. The
/// nested keys are the ones the C ABI's volume info (v2) and volume stats
/// already report.
pub fn envelope(dev: &mut Device, info: &VolumeInfo) -> Result<Json, String> {
    let bitmap = fs_ntfs::bitmap::locate_bitmap_io(dev)?;
    let free_clusters = fs_ntfs::bitmap::count_free_io(dev, &bitmap)?;
    let mft = fs_ntfs::mft_bitmap::locate_io(dev)?;
    let mft_total_records = match &mft.layout {
        fs_ntfs::mft_bitmap::MftBitmapLayout::Resident { total_bits, .. } => *total_bits,
        fs_ntfs::mft_bitmap::MftBitmapLayout::NonResident { total_bits, .. } => *total_bits,
    };
    let mft_free_records = fs_ntfs::mft_bitmap::count_free_io(dev, &mft)?;
    let cluster = u64::from(info.cluster_size);
    Ok(Json::object([
        ("fs", Json::from("ntfs")),
        (
            "label",
            if info.label.is_empty() {
                Json::Null
            } else {
                Json::from(info.label.as_str())
            },
        ),
        ("total_bytes", Json::from(info.total_size)),
        ("free_bytes", Json::from(free_clusters * cluster)),
        ("block_size", Json::from(cluster)),
        ("dirty", Json::from(is_dirty(info))),
        (
            "ntfs",
            Json::object([
                (
                    "serial_number",
                    Json::from(super::mkfs::serial_text(info.serial_number)),
                ),
                (
                    "ntfs_version_major",
                    Json::from(u64::from(info.version_major)),
                ),
                (
                    "ntfs_version_minor",
                    Json::from(u64::from(info.version_minor)),
                ),
                ("volume_flags", Json::from(info.flags)),
                ("bytes_per_sector", Json::from(info.bytes_per_sector)),
                ("cluster_size", Json::from(info.cluster_size)),
                ("mft_record_size", Json::from(info.file_record_size)),
                ("total_clusters", Json::from(info.total_clusters)),
                ("free_clusters", Json::from(free_clusters)),
                ("mft_total_records", Json::from(mft_total_records)),
                ("mft_free_records", Json::from(mft_free_records)),
            ]),
        ),
    ]))
}

fn get(dev: &mut Device, info: &VolumeInfo, key: Option<&str>) -> Result<Outcome, CliError> {
    let all = envelope(dev, info).map_err(|e| CliError::failed(format!("volume: {e}")))?;
    let Some(key) = key else {
        return Ok(Outcome::report(all));
    };
    let mut value = Some(&all);
    for part in key.split('.') {
        value = value.and_then(|v| v.get(part));
    }
    let Some(value) = value else {
        return Err(CliError::usage(format!(
            "no key {key:?}; the keys are {} (and ntfs.<field>)",
            KEYS.join(", ")
        )));
    };
    let text = value.to_text();
    Ok(Outcome::report(Json::object([(key, value.clone())])).with_text(text))
}
