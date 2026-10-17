//! `fs.ntfs <target> <verb>`: an errand inside an NTFS image or device,
//! without mounting it.
//!
//! The verbs are the shared set: `ls`, `stat`, `read`, `write`, `mkdir`,
//! `get`/`info`, `set`, `resize`. Metadata is JSON (or `--text`); file
//! content is raw bytes. A verb the library cannot do yet still exists and
//! answers `not implemented` with exit status 3, so a script moved between
//! filesystems fails loudly instead of meaning something else.

use std::ffi::OsString;
use std::io::Write;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};

use super::device::{self, Device};
use fs_core::cli::{CliError, Json, Outcome, Tool};
use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::read::{self, VolumeInfo};

pub const TOOL: Tool = Tool {
    name: "fs.ntfs",
    verb: "fs",
    section: 1,
    usage_exit: fs_core::cli::output::EXIT_USAGE,
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
        .args(fs_core::cli::format_args().map(|a| a.global(true)))
        .subcommand_required(true)
        .subcommand(
            Cmd::new("acl")
                .about("Inspect a file's persisted security descriptor (read-only)")
                .subcommand_required(true)
                .subcommand(
                    Cmd::new("show")
                        .about("Owner, group, DACL and SACL as stored, ACEs in on-disk order")
                        .arg(
                            Arg::new("path")
                                .value_name("PATH")
                                .default_value("/")
                                .value_parser(value_parser!(OsString)),
                        )
                        .after_help(
                            "Examples:\n  fs.ntfs disk.img acl show /Users\n  \
                             fs.ntfs disk.img acl show /notes.txt | jq '.dacl'\n  \
                             fs.ntfs disk.img acl show --text /",
                        ),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img acl show /Users\n  \
                     fs.ntfs disk.img acl show /notes.txt | jq '.dacl'",
                ),
        )
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
            Cmd::new("stat")
                .about("Report one path: identity, type, links, sizes, every time and NTFS's own fields")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img stat /notes.txt\n  \
                     fs.ntfs disk.img stat /Users | jq .ntfs.file_names\n  \
                     fs.ntfs disk.img stat --text /link\n\n\
                     A symlink or junction is reported, not followed. Times are seconds since \
                     the epoch plus nanoseconds, from $STANDARD_INFORMATION: ctime is the MFT \
                     change and btime the creation. NTFS keeps no POSIX owner or permissions: \
                     uid and gid are null, and mode is the one ls reports for the type.",
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
        .subcommand(
            Cmd::new("rm")
                .about("Remove a file (a directory is removed with rmdir)")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img rm /notes.txt\n  \
                     fs.ntfs disk.img rm /backup/2026/old.tar",
                ),
        )
        .subcommand(
            Cmd::new("rmdir")
                .about("Remove an empty directory")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img rmdir /backup/2026\n\n\
                     The directory must be empty.",
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
                     when the log is empty or records nothing to replay.",
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
        .subcommand(super::inventory::command())
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
        "acl" => {
            let (_, show) = sub.subcommand().expect("clap requires an acl verb");
            let path = path_arg(show)?;
            let (mut dev, _) = device::mount(target, offset, false)?;
            let bytes =
                super::acl::file_descriptor(&mut dev, path).map_err(|e| ntfs_error(path, e))?;
            let json = super::acl::descriptor_json(&bytes)
                .map_err(|e| CliError::failed(format!("{path}: {e}")))?;
            Ok(Outcome::report(json))
        }
        "ls" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            ls(&mut dev, path_arg(sub)?, sub.get_flag("all"))
        }
        "stat" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            let path = path_arg(sub)?;
            stat(&mut dev, path).map_err(|e| ntfs_error(path, e))
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
        "inventory" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            super::inventory::run(&mut dev, sub)
        }
        "set" => set(target, offset, sub),
        "write" => write(target, offset, path_arg(sub)?),
        "mkdir" => mkdir(target, offset, path_arg(sub)?),
        "rm" => remove(target, offset, path_arg(sub)?, false),
        "rmdir" => remove(target, offset, path_arg(sub)?, true),
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

fn ntfs_error(path: &str, e: Error) -> CliError {
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
fn entry(dev: &mut Device, name: &str, record: u64) -> Result<Json, Error> {
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

/// The file attribute bits MS-FSCC 2.6 documents, by the names `stat`
/// gives them. A stored bit not named here is kept, as hex.
const FILE_ATTRIBUTE_NAMES: &[(u32, &str)] = &[
    (0x0000_0001, "readonly"),
    (0x0000_0002, "hidden"),
    (0x0000_0004, "system"),
    (0x0000_0010, "directory"),
    (0x0000_0020, "archive"),
    (0x0000_0080, "normal"),
    (0x0000_0100, "temporary"),
    (0x0000_0200, "sparse_file"),
    (0x0000_0400, "reparse_point"),
    (0x0000_0800, "compressed"),
    (0x0000_1000, "offline"),
    (0x0000_2000, "not_content_indexed"),
    (0x0000_4000, "encrypted"),
    (0x0000_8000, "integrity_stream"),
    (0x0002_0000, "no_scrub_data"),
    (0x0004_0000, "recall_on_open"),
    (0x0008_0000, "pinned"),
    (0x0010_0000, "unpinned"),
    (0x0040_0000, "recall_on_data_access"),
];

fn attribute_names(bits: u32) -> Json {
    let mut names: Vec<Json> = FILE_ATTRIBUTE_NAMES
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| Json::from(*name))
        .collect();
    let named = FILE_ATTRIBUTE_NAMES
        .iter()
        .fold(0, |all, (bit, _)| all | bit);
    if bits & !named != 0 {
        names.push(Json::from(format!("0x{:08x}", bits & !named)));
    }
    Json::Arr(names)
}

/// An NTFS time -- 100 ns units since 1601 -- as whole seconds since the
/// Unix epoch, rounded down, and the nanoseconds past them. The epochs are
/// a whole number of seconds apart, so the fraction is the time's own.
fn unix_time(nt: u64) -> (i64, u64) {
    (read::nt_to_unix(nt), nt % 10_000_000 * 100)
}

/// The four times under the shared keys, from NTFS's creation,
/// modification, MFT change and access times.
fn times(created: u64, modified: u64, changed: u64, accessed: u64) -> Vec<(&'static str, Json)> {
    let mut fields = Vec::new();
    for (key, nsec_key, nt) in [
        ("atime", "atime_nsec", accessed),
        ("mtime", "mtime_nsec", modified),
        ("ctime", "ctime_nsec", changed),
        ("btime", "btime_nsec", created),
    ] {
        let (secs, nsec) = unix_time(nt);
        fields.push((key, Json::from(secs)));
        fields.push((nsec_key, Json::from(nsec)));
    }
    fields
}

/// The records holding a file's attributes of `kind`: its base record,
/// and every extension record its `$ATTRIBUTE_LIST` names for that kind.
fn records_holding(dev: &mut Device, record: u64, kind: AttrType) -> Result<Vec<u64>, Error> {
    let mut records = vec![record];
    if let Some(list) =
        read::read_attribute_value_if_present(dev, record, AttrType::AttributeList, None)?
    {
        for entry in read::parse_attribute_list(&list)? {
            if entry.type_code == kind as u32 && !records.contains(&entry.record_number) {
                records.push(entry.record_number);
            }
        }
    }
    Ok(records)
}

/// Every `$FILE_NAME` of a file: the name, its parent's record and
/// sequence, its namespace and its own four times. The layout is the
/// Linux-NTFS documentation's: the parent reference at 0x00; the creation,
/// modification, MFT change and access times at 0x08, 0x10, 0x18 and 0x20;
/// the name's length in UTF-16 units at 0x40, its namespace at 0x41 and
/// the name itself from 0x42.
fn file_names(dev: &mut Device, record: u64) -> Result<Vec<Json>, Error> {
    let mut names = Vec::new();
    for holder in records_holding(dev, record, AttrType::FileName)? {
        let (_, bytes) = fs_ntfs::mft_io::read_mft_record_io(dev, holder)?;
        for loc in attr_io::iter_attributes(&bytes) {
            if loc.type_code != AttrType::FileName as u32 {
                continue;
            }
            let value = attr_io::resident_value_start(&loc)
                .zip(loc.resident_value_length)
                .and_then(|(start, len)| bytes.get(start..start + len as usize))
                .filter(|v| v.len() >= 0x42)
                .ok_or_else(|| Error::io(format!("record {holder}: a $FILE_NAME is cut short")))?;
            let units: Vec<u16> = value
                .get(0x42..0x42 + 2 * usize::from(value[0x40]))
                .ok_or_else(|| {
                    Error::io(format!("record {holder}: a $FILE_NAME's name is cut short"))
                })?
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let u64_at = |off: usize| u64::from_le_bytes(value[off..off + 8].try_into().unwrap());
            let parent = u64_at(0x00);
            let namespace = match value[0x41] {
                0 => "posix".to_string(),
                1 => "win32".to_string(),
                2 => "dos".to_string(),
                3 => "win32_and_dos".to_string(),
                other => format!("0x{other:02x}"),
            };
            let mut fields = vec![
                ("name", Json::from(String::from_utf16_lossy(&units))),
                ("parent", Json::from(parent & 0xFFFF_FFFF_FFFF)),
                ("parent_sequence", Json::from(parent >> 48)),
                ("namespace", Json::from(namespace)),
            ];
            fields.extend(times(
                u64_at(0x08),
                u64_at(0x10),
                u64_at(0x18),
                u64_at(0x20),
            ));
            names.push(Json::object(fields));
        }
    }
    Ok(names)
}

/// The bytes of clusters the unnamed `$DATA` holds: the allocated length
/// in the header of its first segment, or, for a compressed or sparse
/// stream, the total it allocates (the Win32 ATTRIBUTE_RECORD_HEADER
/// devnote's AllocatedLength at 0x28 and TotalAllocated at 0x40). Data
/// resident in the record, and a directory, hold none.
fn allocated_bytes(dev: &mut Device, record: u64) -> Result<u64, Error> {
    const COMPRESSED: u16 = 0x0001;
    const SPARSE: u16 = 0x8000;
    for holder in records_holding(dev, record, AttrType::Data)? {
        let (_, bytes) = fs_ntfs::mft_io::read_mft_record_io(dev, holder)?;
        for loc in attr_io::iter_attributes(&bytes) {
            if loc.type_code != AttrType::Data as u32 || loc.name_length != 0 {
                continue;
            }
            if loc.is_resident {
                return Ok(0);
            }
            let header = bytes
                .get(loc.attr_offset..loc.attr_offset + loc.attr_length)
                .filter(|h| h.len() >= 0x40)
                .ok_or_else(|| {
                    Error::io(format!("record {holder}: $DATA's header is cut short"))
                })?;
            let u64_at = |off: usize| u64::from_le_bytes(header[off..off + 8].try_into().unwrap());
            if u64_at(0x10) != 0 {
                continue; // a later segment: only the first carries the sizes
            }
            let flags = u16::from_le_bytes([header[0x0C], header[0x0D]]);
            let total_present = loc.non_resident_mapping_pairs_offset.unwrap_or(0) >= 0x48;
            return Ok(if flags & (COMPRESSED | SPARSE) != 0 && total_present {
                u64_at(0x40)
            } else {
                u64_at(0x28)
            });
        }
    }
    Ok(0)
}

/// The bytes of a directory's `$I30` index allocation: the data length in
/// the header of its first `$INDEX_ALLOCATION` segment, which is the size
/// Windows reports for a directory. An index that fits its `$INDEX_ROOT`
/// has none, and Windows reports 0.
fn index_bytes(dev: &mut Device, record: u64) -> Result<u64, Error> {
    for holder in records_holding(dev, record, AttrType::IndexAllocation)? {
        let (_, bytes) = fs_ntfs::mft_io::read_mft_record_io(dev, holder)?;
        for loc in attr_io::iter_attributes(&bytes) {
            if loc.type_code != AttrType::IndexAllocation as u32 || loc.is_resident {
                continue;
            }
            let start = loc.attr_offset + usize::from(loc.name_offset);
            let name: Vec<u16> = bytes
                .get(start..start + 2 * usize::from(loc.name_length))
                .unwrap_or_default()
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let first = bytes
                .get(loc.attr_offset + 0x10..loc.attr_offset + 0x18)
                .is_some_and(|v| v.iter().all(|b| *b == 0));
            if String::from_utf16_lossy(&name) == "$I30" && first {
                return Ok(loc.non_resident_value_length.unwrap_or(0));
            }
        }
    }
    Ok(0)
}

/// `stat PATH`: one path's identity, type, links, sizes and every time,
/// the shared keys first and NTFS's own under `ntfs`. A symlink or
/// junction is reported, not followed.
///
/// NTFS keeps no POSIX owner or permissions, so `uid` and `gid` are null
/// and `mode` is the one `ls` reports for the type. `nlink` counts the
/// file's names, a DOS 8.3 alias not being a link of its own. The times
/// are `$STANDARD_INFORMATION`'s, `ctime` its MFT change and `btime` its
/// creation; each `$FILE_NAME` keeps its own four under `ntfs.file_names`.
fn stat(dev: &mut Device, path: &str) -> Result<Outcome, Error> {
    let record = read::resolve_path(dev, path)?;
    let st = read::read_stat(dev, record)?;
    let (_, base) = fs_ntfs::mft_io::read_mft_record_io(dev, record)?;
    let reparse = read::read_attribute_value_if_present(dev, record, AttrType::ReparsePoint, None)?;
    let tag = reparse
        .as_deref()
        .filter(|r| r.len() >= 4)
        .map(|r| u32::from_le_bytes([r[0], r[1], r[2], r[3]]));
    let kind = kind(st.is_dir, tag);
    let names = file_names(dev, record)?;
    let links = names
        .iter()
        .filter(|n| n.get("namespace").map(Json::to_text).as_deref() != Some("dos"))
        .count() as u64;
    let security_id = fs_ntfs::write::read_si_full_io(dev, path)?
        .v3
        .map(|v3| v3.security_id);
    let object_id = read::read_attribute_value_if_present(dev, record, AttrType::ObjectId, None)?
        .and_then(|v| {
            v.get(..16)
                .map(|id| id.iter().map(|b| format!("{b:02x}")).collect::<String>())
        });
    let named_streams = fs_ntfs::write::list_named_streams_io(dev, path)?.len() as u64;
    let ea_count = match read::read_attribute_value_if_present(
        dev,
        record,
        AttrType::ExtendedAttribute,
        None,
    )? {
        Some(value) => fs_ntfs::ea_io::decode(&value)?.len() as u64,
        None => 0,
    };
    // NTFS marks a file holding EAs with 0x40000 in $STANDARD_INFORMATION,
    // the bit winnt.h names FILE_ATTRIBUTE_EA; Windows never reports it for
    // such a file (32, not 262176, for the fixture's /ea.txt).
    const FILE_ATTRIBUTE_EA: u32 = 0x0004_0000;
    let ea_bit = if ea_count > 0 { FILE_ATTRIBUTE_EA } else { 0 };
    let attributes = (st.file_attributes & !ea_bit) | if st.is_dir { 0x10 } else { 0 };
    // A directory's size is its $I30 allocation's, as Windows reports it.
    let size = if st.is_dir {
        index_bytes(dev, record)?
    } else {
        st.size
    };

    let mut fields = vec![
        ("path", Json::from(path)),
        ("inode", Json::from(record)),
        ("type", Json::from(kind)),
        ("mode", Json::from(mode(kind))),
        ("uid", Json::Null),
        ("gid", Json::Null),
        ("nlink", Json::from(links)),
        ("size", Json::from(size)),
        ("allocated_bytes", Json::from(allocated_bytes(dev, record)?)),
    ];
    fields.extend(times(
        st.created_nt,
        st.modified_nt,
        st.mft_modified_nt,
        st.accessed_nt,
    ));
    if matches!(kind, "symlink" | "junction") {
        fields.push((
            "target",
            Json::from(reparse.as_deref().and_then(fs_ntfs::reparse_link_target)),
        ));
    }
    fields.push((
        "ntfs",
        Json::object([
            ("record_number", Json::from(record)),
            (
                "sequence",
                Json::from(fs_ntfs::mft_io::record_sequence(&base)),
            ),
            ("link_count", Json::from(st.link_count)),
            ("attributes", attribute_names(attributes)),
            ("security_id", Json::from(security_id)),
            ("reparse_tag", Json::from(tag.map(|t| format!("0x{t:08x}")))),
            ("object_id", Json::from(object_id)),
            ("named_streams", Json::from(named_streams)),
            ("ea_count", Json::from(ea_count)),
            ("file_names", Json::Arr(names)),
        ]),
    ));
    Ok(Outcome::report(Json::object(fields)))
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

/// Run `edit` on a writable open of `target`, then sync it. The volume is
/// prepared as the C ABI's read-write mount prepares it: a dirty volume, a
/// log that cannot be read, and a `$LogFile` holding work are refused
/// before anything is written (#137). Windows leaves a log holding work
/// after a Fast Startup shutdown as well as after a crash, so the replay is
/// the user's to ask for, with `fsck.ntfs -y`. A fresh-format
/// volume is upgraded first, as Windows does on its first read-write mount,
/// and a change journal is retired, since the edit will not be in it (#479).
fn edit<T>(
    target: &OsString,
    offset: u64,
    what: &str,
    edit: impl FnOnce(&mut Device) -> Result<T, Error>,
) -> Result<T, CliError> {
    let (mut dev, _) = device::mount(target, offset, true)?;
    fs_ntfs::fsck::prepare_rw_mount_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: {e}")))?;
    fs_ntfs::fsck::upgrade_volume_version_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: upgrade the volume version: {e}")))?;
    fs_ntfs::fsck::retire_usn_journal_io(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: retire the change journal: {e}")))?;
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
                    return Err(Error::is_directory("is a directory"));
                }
                false
            }
            Err(e) if e.kind() == Kind::NotFound => {
                let parent_record = read::resolve_path(dev, &parent)?;
                if !read::read_stat(dev, parent_record)?.is_dir {
                    return Err(Error::not_directory(format!("{parent} is not a directory")));
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
                    .and_then(|()| Ok(fs_ntfs::block_io::BlockIo::sync(dev)?));
                return Err(match undone {
                    Ok(()) => e.map_message(|m| format!("{m} (the new file was removed again)")),
                    Err(u) => {
                        e.map_message(|m| format!("{m}; and removing the new file failed: {u}"))
                    }
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
            return Err(Error::exists("already exists"));
        }
        fs_ntfs::write::mkdir_io(dev, &parent, name)
    })?;
    let report = Json::object([("path", Json::from(path)), ("record", Json::from(record))]);
    Ok(Outcome::report(report).with_text(format!("created {path}")))
}

/// Remove a file (`rm`) or an empty directory (`rmdir`). Each refuses the
/// other's kind of entry, as `unlink` and `rmdir` do, and everything that
/// can be refused is refused before anything is written.
fn remove(
    target: &OsString,
    offset: u64,
    path: &str,
    directory: bool,
) -> Result<Outcome, CliError> {
    split_path(path)?;
    edit(target, offset, path, |dev| {
        if directory {
            fs_ntfs::write::rmdir_io(dev, path)
        } else {
            fs_ntfs::write::unlink_io(dev, path)
        }
    })?;
    let report = Json::object([("path", Json::from(path))]);
    Ok(Outcome::report(report).with_text(format!("removed {path}")))
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
    // The records `$MFT` holds and the bitmap describes, as the free
    // count below is: Windows sizes `$MFT:$Bitmap` far ahead of `$MFT`,
    // so the bitmap's own length is not a record count (#415).
    let mft_total_records = mft.record_limit();
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
