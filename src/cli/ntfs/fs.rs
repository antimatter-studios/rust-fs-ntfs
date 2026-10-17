//! `fs.ntfs <target> <verb>`: an errand inside an NTFS image or device,
//! without mounting it.
//!
//! The verbs include `ls`, `stat`, `streams`, `read`, `write`, `mkdir`,
//! `truncate`, `rm`, `rmdir`, `mv`, `ln`, `get`/`info`, `set`, `resize`. Metadata is JSON
//! (or `--text`); file content is raw bytes. A verb the library cannot do
//! yet still exists and answers `not implemented` with exit status 3, so a
//! script moved between filesystems fails loudly instead of meaning
//! something else.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Write;

use clap::{value_parser, Arg, ArgAction, ArgMatches, Command as Cmd};

use super::device::{self, Device};
use fs_core::cli::{CliError, Format, Json, Outcome, Tool};
use fs_ntfs::attr_io::{self, AttrType};
use fs_ntfs::block_io::BlockIo;
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
                    Arg::new("recursive").long("recursive").short('R')
                        .action(ArgAction::SetTrue)
                        .help("Walk depth first, names sorted as bytes; never follow a reparse point"),
                )
                .arg(
                    Arg::new("max-depth").long("max-depth").value_name("N")
                        .value_parser(value_parser!(u64)).requires("recursive")
                        .help("How deep --recursive may go (default: 64); a directory's entries are depth 1"),
                )
                .arg(
                    Arg::new("limit").long("limit").value_name("N")
                        .value_parser(value_parser!(u64)).requires("recursive")
                        .help("How many entries --recursive may list (default: 100000)"),
                )
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
                     fs.ntfs disk.img ls --all --text /\n  \
                     fs.ntfs disk.img ls -R --max-depth 8 --limit 1000 /Users\n\n\
                     With --recursive each entry adds its path from the root of the volume and \
                     its depth: PATH's entries are depth 1. A non-directory or reparse PATH is \
                     listed alone at depth 0. Symlinks, junctions and all other reparse points \
                     are never entered. An already visited directory record is listed with \
                     revisited: true and not entered again. A tree that exactly fits succeeds; \
                     one more entry past a bound, or an unreadable directory, exits 1 with \
                     the entries so far on stdout and the bound or path named on stderr. \
                     --limit 0 allows no entry; --max-depth 0 none below PATH. \
                     Plain ls lists only the directory's entries, without path or depth.",
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
            Cmd::new("streams")
                .about("List named data streams and their sizes and storage flags (read-only)")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img streams /notes.txt\n  \
                     fs.ntfs disk.img streams --text /Users\n  \
                     fs.ntfs --offset 1048576 whole-disk.img streams /notes.txt\n\n\
                     Only named $DATA streams are listed; the unnamed stream is excluded. \
                     Each row reports name, size, allocated, resident, sparse and compressed. \
                     Allocation is bytes reserved for the stream, including resident value \
                     space in its record. Directories can have named streams too. A link's \
                     own streams are listed without following it. PATH is always a path: \
                     no colon suffix is interpreted as a stream selector.",
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
                .about("Create or replace a file, or patch an existing file with --at")
                .arg(
                    Arg::new("at")
                        .long("at")
                        .value_name("BYTES")
                        .value_parser(value_parser!(u64))
                        .help("Write stdin at this byte in an existing regular file, without truncating"),
                )
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img write /notes.txt < notes.txt\n  \
                     tar cf - ./dir | fs.ntfs disk.img write /backup.tar\n  \
                     fs.ext4 src.img read /f | fs.ntfs dst.img write /f\n  \
                     fs.ntfs disk.img write --at 4095 /notes.txt < patch.bin\n\n\
                     Plain write creates or replaces; the parent must exist. With --at, \
                     PATH must already be a regular file. Its prefix and suffix are kept; \
                     a write past EOF grows the file and fills any gap with zeros. Empty \
                     input changes nothing, even past EOF. Directories, missing files and \
                     read-only files fail; symbolic links and other reparse points are \
                     never followed (exit 3). Compressed, sparse or encrypted data the \
                     driver cannot patch also answer exit 3. Input is read completely \
                     before opening TARGET, and the edit is rehearsed before a writable \
                     open, so predictable refusals leave the image unchanged. --at is a \
                     byte position in the file; --offset remains the filesystem's position \
                     in TARGET. Counts are unsigned decimal bytes without suffixes.",
                ),
        )
        .subcommand(
            Cmd::new("truncate")
                .about("Set a regular file's length in bytes; growth reads back as zeros")
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .arg(
                    Arg::new("size")
                        .value_name("SIZE")
                        .required(true)
                        .value_parser(value_parser!(u64))
                        .help("The new length in bytes, an unsigned decimal integer"),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img truncate /notes.txt 0\n  \
                     fs.ntfs disk.img truncate /backup.bin 1048576\n  \
                     fs.ntfs --offset 1048576 whole-disk.img truncate /notes.txt 4097\n\n\
                     Shrinking releases whole clusters past the new end; growing preserves \
                     the prefix and reads back zeros in the new tail. Resident files can \
                     grow into non-resident files. A directory, symbolic link or other \
                     reparse point is refused; links are never followed. Compressed, sparse \
                     and encrypted data this driver cannot resize answer exit 3. Every \
                     edit is rehearsed before the image is opened for writing, so a \
                     refused command leaves every byte unchanged. SIZE has no suffix; \
                     --offset is the filesystem's offset in TARGET, never a file offset.",
                ),
        )
        .subcommand(
            Cmd::new("mkdir")
                .about("Create a directory (its parent must exist, or with -p every missing one)")
                .arg(
                    Arg::new("parents")
                        .short('p')
                        .long("parents")
                        .action(ArgAction::SetTrue)
                        .help("Make every missing directory on the way to PATH; one already there is fine"),
                )
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img mkdir /backup\n  \
                     fs.ntfs disk.img mkdir /backup/2026\n  \
                     fs.ntfs disk.img mkdir -p /backup/2026/october/photos\n\n\
                     Without -p, PATH's parent must exist and nothing may be at PATH. With \
                     -p, every missing directory on the way to PATH is made, and a directory \
                     already at PATH, or at any step, is fine: the report lists only the \
                     directories made, {\"path\": ..., \"created\": [...]}. Names are found as \
                     Windows finds them, without regard to case, and . and .. are taken from \
                     the text of PATH as Windows takes them, so /a/../b makes only /b. A file \
                     or a symbolic link on the way or at PATH, a name Windows cannot address \
                     and a dirty volume are refused before the image is opened for writing, so \
                     a refused mkdir -p makes no part of the chain.",
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
        .subcommand(
            Cmd::new("mv")
                .about("Give a file or directory a new name in the same directory")
                .arg(
                    Arg::new("source")
                        .value_name("SOURCE")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img mv /notes.txt /notes-2026.txt\n  \
                     fs.ntfs disk.img mv /backup/old.tar /backup/2025.tar\n  \
                     fs.ntfs disk.img mv /readme.txt /README.txt\n\n\
                     PATH is the new name itself, not a directory to move into, and must be \
                     in SOURCE's directory: a move to another directory answers `not \
                     implemented` (exit 3). A name already at PATH is refused, never \
                     replaced, and names are compared as Windows compares them, without \
                     regard to case; a case-only rename of SOURCE itself is allowed. A \
                     missing SOURCE, a name Windows cannot address, the root, . and .. and \
                     the volume's own metafiles are refused before the image is opened for \
                     writing.",
                ),
        )
        .subcommand(
            Cmd::new("ln")
                .about("Add a name for a file (a hard link), or with -s make a symbolic link")
                .arg(
                    Arg::new("symbolic")
                        .short('s')
                        .long("symbolic")
                        .action(ArgAction::SetTrue)
                        .help("Make a symbolic link whose target is TARGET, as given"),
                )
                .arg(
                    Arg::new("target")
                        .value_name("TARGET")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .arg(
                    Arg::new("path")
                        .value_name("PATH")
                        .required(true)
                        .value_parser(value_parser!(OsString)),
                )
                .after_help(
                    "Examples:\n  fs.ntfs disk.img ln /data/report.txt /data/report-2026.txt\n  \
                     fs.ntfs disk.img ln /data/report.txt /archive/report.txt\n  \
                     fs.ntfs disk.img ln -s ../data/report.txt /docs/report.txt\n  \
                     fs.ntfs disk.img ln -s /data/report.txt /report.txt\n\n\
                     Without -s, PATH becomes another name for the file at TARGET, in any \
                     directory. A directory is given no second name, and a file has at most \
                     the 1024 names Windows allows. With -s, PATH is a symbolic link to TARGET, \
                     the text as given: relative to PATH's directory, from the volume's root \
                     when it starts with /, or a drive path such as C:\\data. The link is a \
                     file link, as mklink makes one without /D. A name already at PATH, \
                     compared as Windows compares names, a missing directory, a name Windows \
                     cannot address and the volume's own metafiles are refused before the \
                     image is opened for writing.",
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
            if sub.get_flag("recursive") {
                let format = Format::of(matches, &std::env::args_os().collect::<Vec<_>>());
                ls_recursive(&mut dev, path_arg(sub)?, sub, format)
            } else {
                ls(&mut dev, path_arg(sub)?, sub.get_flag("all"))
            }
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
        "streams" => {
            let (mut dev, _) = device::mount(target, offset, false)?;
            let path = path_arg(sub)?;
            streams(&mut dev, path).map_err(|e| ntfs_error(path, e))
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
        "write" => match sub.get_one::<u64>("at") {
            Some(at) => write_at(target, offset, path_arg(sub)?, *at),
            None => write(target, offset, path_arg(sub)?),
        },
        "mkdir" if sub.get_flag("parents") => mkdir_parents(target, offset, path_arg(sub)?),
        "mkdir" => mkdir(target, offset, path_arg(sub)?),
        "truncate" => truncate(
            target,
            offset,
            path_arg(sub)?,
            *sub.get_one::<u64>("size").expect("clap requires size"),
        ),
        "rm" => remove(target, offset, path_arg(sub)?, false),
        "rmdir" => remove(target, offset, path_arg(sub)?, true),
        "mv" => rename(target, offset, text_arg(sub, "source")?, path_arg(sub)?),
        "ln" => link(
            target,
            offset,
            text_arg(sub, "target")?,
            path_arg(sub)?,
            sub.get_flag("symbolic"),
        ),
        "resize" => Err(CliError::not_implemented(
            "resize: this library cannot resize an NTFS volume",
        )),
        other => unreachable!("clap knows no verb {other}"),
    }
}

/// A path argument. The library takes paths as text, so one that is not
/// UTF-8 cannot name anything on an NTFS volume (whose names are UTF-16).
fn path_arg(sub: &ArgMatches) -> Result<&str, CliError> {
    text_arg(sub, "path")
}

/// The argument `id` as a path; see [`path_arg`].
fn text_arg<'a>(sub: &'a ArgMatches, id: &str) -> Result<&'a str, CliError> {
    let path = sub
        .get_one::<OsString>(id)
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
    entry_info(dev, name, record).map(|(item, _)| item)
}

/// The existing entry and whether it is a directory safe to enter. Even
/// an unknown reparse tag, or a reparse flag without its data, is a leaf.
fn entry_info(dev: &mut Device, name: &str, record: u64) -> Result<(Json, bool), Error> {
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
    let descend = st.is_dir && reparse.is_none() && st.file_attributes & 0x400 == 0;
    Ok((Json::object(fields), descend))
}

fn entry_text(e: &Json) -> String {
    let field = |k: &str| e.get(k).map(Json::to_text).unwrap_or_default();
    let mut line = format!(
        "{}{} {:>12} {}",
        type_char(&field("type")),
        field("mode"),
        field("size"),
        e.get("path")
            .map(Json::to_text)
            .unwrap_or_else(|| field("name"))
    );
    if let Some(target) = e.get("target") {
        line.push_str(&format!(" -> {}", target.to_text()));
    }
    if e.get("revisited") == Some(&Json::Bool(true)) {
        line.push_str(" [revisited]");
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

/// The remaining children of one directory, in byte order. Dot entries
/// never participate in a recursive listing, including with --all.
fn sorted_children(
    dev: &mut Device,
    record: u64,
    path: &str,
    all: bool,
) -> Result<std::vec::IntoIter<read::DirEntry>, CliError> {
    let mut children = read::read_dir_entries(dev, record).map_err(|e| ntfs_error(path, e))?;
    children.retain(|e| {
        e.name != "." && e.name != ".." && (all || e.record_number >= FIRST_USER_RECORD)
    });
    children.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Ok(children.into_iter())
}

fn recursive_entry(item: Json, path: String, depth: u64, revisited: bool) -> Json {
    let Json::Obj(mut fields) = item else {
        unreachable!("an ls entry is an object")
    };
    fields.push(("path".into(), Json::from(path)));
    fields.push(("depth".into(), Json::from(depth)));
    if revisited {
        fields.push(("revisited".into(), Json::from(true)));
    }
    Json::Obj(fields)
}

/// An iterative depth-first walk: one directory's remaining entries per
/// level on the heap, with directory records visited globally. Bounds
/// refuse the next entry, so exact fits (including empty trees) succeed.
fn ls_recursive(
    dev: &mut Device,
    path: &str,
    sub: &ArgMatches,
    format: Format,
) -> Result<Outcome, CliError> {
    let record = read::resolve_path(dev, path).map_err(|e| ntfs_error(path, e))?;
    let start = format!(
        "/{}",
        path.split('/')
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    );
    let (first, descend) = entry_info(dev, start.rsplit('/').next().unwrap_or(""), record)
        .map_err(|e| ntfs_error(path, e))?;
    let max_depth = sub.get_one::<u64>("max-depth").copied().unwrap_or(64);
    let limit = sub.get_one::<u64>("limit").copied().unwrap_or(100_000);
    let all = sub.get_flag("all");
    let reached = |bound: &str, value: u64, at: &str| {
        CliError::failed(format!("{bound} {value} reached at {at}"))
    };
    let mut entries = Vec::new();
    let walked = (|| {
        if !descend {
            if limit == 0 {
                return Err(reached("--limit", limit, &start));
            }
            entries.push(recursive_entry(first, start, 0, false));
            return Ok(());
        }
        let mut visited = HashSet::from([record]);
        let mut stack = vec![(
            start.clone(),
            0u64,
            sorted_children(dev, record, &start, all)?,
        )];
        while let Some((parent, parent_depth, children)) = stack.last_mut() {
            let Some(child) = children.next() else {
                stack.pop();
                continue;
            };
            let path = format!("{}/{}", parent.trim_end_matches('/'), child.name);
            let depth = *parent_depth + 1;
            if entries.len() as u64 >= limit {
                return Err(reached("--limit", limit, &path));
            }
            if depth > max_depth {
                return Err(reached("--max-depth", max_depth, &path));
            }
            let (item, descend) = entry_info(dev, &child.name, child.record_number)
                .map_err(|e| ntfs_error(&path, e))?;
            let revisited = descend && !visited.insert(child.record_number);
            entries.push(recursive_entry(item, path.clone(), depth, revisited));
            if descend && !revisited {
                let next = sorted_children(dev, child.record_number, &path, all)?;
                stack.push((path, depth, next));
            }
        }
        Ok(())
    })();
    let text = entries
        .iter()
        .map(entry_text)
        .collect::<Vec<_>>()
        .join("\n");
    let report = Outcome::report(Json::Arr(entries)).with_text(text);
    match walked {
        Ok(()) => Ok(report),
        Err(error) => {
            // The shared outcome carries a report or an error, so render
            // the complete partial report before returning the error.
            let partial = fs_core::cli::output::render(TOOL.name, format, Ok(report));
            let mut out = std::io::stdout().lock();
            out.write_all(partial.stdout.as_bytes())
                .and_then(|()| out.flush())
                .map_err(|e| CliError::failed(format!("write: {e}")))?;
            Err(error)
        }
    }
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

/// Enumerate headers only, including attributes in extension records. A
/// split stream contributes its first segment, the only one with sizes.
/// The layout is Microsoft's ATTRIBUTE_RECORD_HEADER devnote; allocation
/// is compared with Windows FileStreamInfo, including resident padding.
fn streams(dev: &mut Device, path: &str) -> Result<Outcome, Error> {
    let record = read::resolve_path(dev, path)?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for holder in records_holding(dev, record, AttrType::Data)? {
        let (_, bytes) = fs_ntfs::mft_io::read_mft_record_io(dev, holder)?;
        for loc in attr_io::iter_attributes(&bytes) {
            if loc.type_code != AttrType::Data as u32 || loc.name_length == 0 {
                continue;
            }
            let header = &bytes[loc.attr_offset..loc.attr_offset + loc.attr_length];
            let u64_at = |off: usize| u64::from_le_bytes(header[off..off + 8].try_into().unwrap());
            if !loc.is_resident && u64_at(0x10) != 0 {
                continue;
            }
            let name = attr_io::decode_attr_name(&bytes, &loc);
            if !seen.insert(name.clone()) {
                continue;
            }
            let flags = u16::from_le_bytes([header[0x0C], header[0x0D]]);
            let sparse = flags & attr_io::attr_flags::SPARSE != 0;
            let compressed = flags & attr_io::attr_flags::COMPRESSION_MASK != 0;
            let (size, allocated) = if loc.is_resident {
                (
                    loc.resident_value_length.unwrap_or(0) as u64,
                    (loc.attr_length - loc.resident_value_offset.unwrap_or(0) as usize) as u64,
                )
            } else {
                let extended = loc.non_resident_mapping_pairs_offset.unwrap_or(0) >= 0x48;
                (
                    loc.non_resident_value_length.unwrap_or(0),
                    if (sparse || compressed) && extended {
                        u64_at(0x40)
                    } else {
                        u64_at(0x28)
                    },
                )
            };
            let row = Json::object([
                ("name", Json::from(name.clone())),
                ("size", Json::from(size)),
                ("allocated", Json::from(allocated)),
                ("resident", Json::from(loc.is_resident)),
                ("sparse", Json::from(sparse)),
                ("compressed", Json::from(compressed)),
            ]);
            rows.push((name, row));
        }
    }
    rows.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(Outcome::report(Json::Arr(
        rows.into_iter().map(|(_, row)| row).collect(),
    )))
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
    let done = prepared(&mut dev, what, edit)?;
    fs_ntfs::block_io::BlockIo::sync(&mut dev)
        .map_err(|e| CliError::failed(format!("{what}: {e}")))?;
    Ok(done)
}

/// What [`edit`] does to an open volume, before it syncs: the preparation,
/// then `edit`. Over a [`device::Rehearsal`] it is a rehearsal of the edit.
fn prepared<D: BlockIo, T>(
    dev: &mut D,
    what: &str,
    edit: impl FnOnce(&mut D) -> Result<T, Error>,
) -> Result<T, CliError> {
    fs_ntfs::fsck::prepare_rw_mount_io(dev)
        .map_err(|e| CliError::failed(format!("{what}: {e}")))?;
    fs_ntfs::fsck::upgrade_volume_version_io(dev)
        .map_err(|e| CliError::failed(format!("{what}: upgrade the volume version: {e}")))?;
    fs_ntfs::fsck::retire_usn_journal_io(dev)
        .map_err(|e| CliError::failed(format!("{what}: retire the change journal: {e}")))?;
    edit(dev).map_err(|e| CliError::failed(format!("{what}: {e}")))
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

/// Patch an existing file through the driver's positional writer. Like
/// Windows WriteFile, a nonempty write grows to its end with a zero gap;
/// zero bytes neither extend nor truncate. All predictable failures are
/// rehearsed, including mount preparation, before any writable open.
fn write_at(target: &OsString, offset: u64, path: &str, at: u64) -> Result<Outcome, CliError> {
    last_name(path)?;
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut data)
        .map_err(|e| CliError::failed(format!("read stdin: {e}")))?;
    let (mut dev, info) = device::mount(target, offset, false)?;
    let record = read::resolve_path(&mut dev, path).map_err(|e| ntfs_error(path, e))?;
    let st = read::read_stat(&mut dev, record).map_err(|e| ntfs_error(path, e))?;
    if st.file_attributes & 0x400 != 0
        || read::read_attribute_value_if_present(&mut dev, record, AttrType::ReparsePoint, None)
            .map_err(|e| ntfs_error(path, e))?
            .is_some()
    {
        return Err(CliError::not_implemented(format!(
            "{path}: write --at does not follow symbolic links or other reparse points"
        )));
    }
    if st.is_dir || path.ends_with('/') {
        return Err(CliError::failed(format!("{path}: not a regular file")));
    }
    if st.file_attributes & fs_ntfs::write::file_attr::READONLY != 0 {
        return Err(CliError::failed(format!("{path}: file is read-only")));
    }
    let (_, record_bytes) =
        fs_ntfs::mft_io::read_mft_record_io(&mut dev, record).map_err(|e| ntfs_error(path, e))?;
    let attr = attr_io::find_attribute(&record_bytes, AttrType::Data, None).ok_or_else(|| {
        CliError::not_implemented(format!(
            "{path}: no unnamed $DATA in the base record to patch"
        ))
    })?;
    let flags = attr_io::attribute_flags(&record_bytes, &attr)
        .ok_or_else(|| CliError::failed(format!("{path}: truncated $DATA flags")))?;
    if flags & attr_io::attr_flags::TRANSFORMED != 0 {
        return Err(CliError::not_implemented(format!(
            "{path}: write --at cannot patch {} data",
            attr_io::describe_transform_flags(flags)
        )));
    }
    let bytes = data.len() as u64;
    let size = if data.is_empty() {
        st.size
    } else {
        st.size.max(at.checked_add(bytes).ok_or_else(|| {
            CliError::failed(format!("{path}: position plus input length overflows"))
        })?)
    };
    if size > info.total_size {
        return Err(CliError::failed(format!(
            "{path}: no space for {size} bytes on this volume"
        )));
    }
    let mut rehearsal = device::Rehearsal::over(&mut dev);
    prepared(&mut rehearsal, path, |_| Ok(()))?;
    if data.is_empty() {
        // write_at_io's empty fast path bypasses its metafile guard. A
        // same-size truncate rehearses that policy without touching disk.
        fs_ntfs::write::truncate_io(&mut rehearsal, path, st.size)
            .map_err(|e| ntfs_error(path, e))?;
    } else {
        fs_ntfs::write::write_at_io(&mut rehearsal, path, at, &data).map_err(|e| {
            if e.contains("exceed attr capacity") {
                CliError::not_implemented(format!("{path}: {e}"))
            } else {
                ntfs_error(path, e)
            }
        })?;
        edit(target, offset, path, |io| {
            fs_ntfs::write::write_at_io(io, path, at, &data)
        })?;
    }
    let report = Json::object([
        ("path", Json::from(path)),
        ("at", Json::from(at)),
        ("bytes", Json::from(bytes)),
        ("size", Json::from(size)),
    ]);
    Ok(Outcome::report(report)
        .with_text(format!("{path}: wrote {bytes} bytes at {at}, size {size}")))
}

/// Rehearse the complete resize over a read-only open, including mount
/// preparation. Unsupported data shapes and all predictable failures are
/// refused before a writable open, even if growth would allocate and then
/// discover that the new mapping pairs do not fit in the record.
fn truncate(target: &OsString, offset: u64, path: &str, size: u64) -> Result<Outcome, CliError> {
    last_name(path)?;
    let (mut dev, info) = device::mount(target, offset, false)?;
    let record = read::resolve_path(&mut dev, path).map_err(|e| ntfs_error(path, e))?;
    let st = read::read_stat(&mut dev, record).map_err(|e| ntfs_error(path, e))?;
    if st.file_attributes & 0x400 != 0
        || read::read_attribute_value_if_present(&mut dev, record, AttrType::ReparsePoint, None)
            .map_err(|e| ntfs_error(path, e))?
            .is_some()
    {
        return Err(CliError::not_implemented(format!(
            "{path}: truncate does not follow symbolic links or other reparse points"
        )));
    }
    if st.is_dir || path.ends_with('/') {
        return Err(CliError::failed(format!("{path}: not a regular file")));
    }
    let (_, bytes) =
        fs_ntfs::mft_io::read_mft_record_io(&mut dev, record).map_err(|e| ntfs_error(path, e))?;
    let data = attr_io::find_attribute(&bytes, AttrType::Data, None).ok_or_else(|| {
        CliError::not_implemented(format!(
            "{path}: no unnamed $DATA in the base record to resize"
        ))
    })?;
    let flags = attr_io::attribute_flags(&bytes, &data)
        .ok_or_else(|| CliError::failed(format!("{path}: truncated $DATA flags")))?;
    if flags & attr_io::attr_flags::TRANSFORMED != 0 {
        return Err(CliError::not_implemented(format!(
            "{path}: truncate cannot resize {} data",
            attr_io::describe_transform_flags(flags)
        )));
    }
    if size > info.total_size {
        return Err(CliError::failed(format!(
            "{path}: no space for {size} bytes on this volume"
        )));
    }
    // A same-size request still checks the driver policy (notably metafiles),
    // but makes no writable open and changes no timestamps or volume metadata.
    let mut rehearsal = device::Rehearsal::over(&mut dev);
    if size == st.size {
        fs_ntfs::write::truncate_io(&mut rehearsal, path, size).map_err(|e| ntfs_error(path, e))?;
    } else {
        prepared(&mut rehearsal, path, |_| Ok(()))?;
        fs_ntfs::write::truncate_io(&mut rehearsal, path, size).map_err(|e| {
            if e.contains("exceed attr capacity") {
                CliError::not_implemented(format!("{path}: {e}"))
            } else {
                ntfs_error(path, e)
            }
        })?;
        edit(target, offset, path, |io| {
            fs_ntfs::write::truncate_io(io, path, size)
        })?;
    }
    let report = Json::object([("path", Json::from(path)), ("size", Json::from(size))]);
    Ok(Outcome::report(report).with_text(format!("{path}: {size} bytes")))
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

/// `mkdir --parents`: every missing directory on the way to `path`, made
/// through the library's mkdir in one writable open with one sync. As for
/// `mv` and `ln`, everything that can be refused is refused on a read-only
/// open: first what this verb decides itself, then the whole chain
/// rehearsed, so a refused command makes no part of the chain and leaves
/// the image byte-identical. A chain already there makes nothing and never
/// opens the image for writing.
fn mkdir_parents(target: &OsString, offset: u64, path: &str) -> Result<Outcome, CliError> {
    let missing = missing_directories(target, offset, path)?;
    if !missing.is_empty() {
        edit(target, offset, path, |dev| make_directories(dev, &missing))?;
    }
    let text = if missing.is_empty() {
        format!("{path} exists; nothing created")
    } else {
        let lines: Vec<String> = missing.iter().map(|dir| format!("created {dir}")).collect();
        lines.join("\n")
    };
    let created = missing.iter().map(|dir| Json::from(dir.as_str())).collect();
    let report = Json::object([("path", Json::from(path)), ("created", Json::Arr(created))]);
    Ok(Outcome::report(report).with_text(text))
}

/// The names on the way to `path`, `.` and `..` taken from its text as
/// Windows takes them: `.` stays where it is and `..` drops the name before
/// it (never past the root) before any name is looked up, so `/a/../b` is
/// `/b` whatever `/a` is. A path not from the root is a usage error, as it
/// is for every other verb.
fn path_names(path: &str) -> Result<Vec<&str>, CliError> {
    if !path.starts_with('/') {
        return Err(CliError::usage(format!(
            "{path}: give the path from the root, as /{path}"
        )));
    }
    let mut names = Vec::new();
    for name in path.split('/') {
        match name {
            "" | "." => {}
            ".." => {
                names.pop();
            }
            name => names.push(name),
        }
    }
    Ok(names)
}

/// The directories [`mkdir_parents`] makes, from the root down, named as
/// `path` spells them, once every refusal has been ruled out. In order:
///
/// - a name already there, found as Windows finds names (through
///   `$UpCase`), that is not a directory to enter: a file, or a symbolic
///   link, junction or other reparse point, none of which is followed --
///   "not a directory" on the way to `path`, "already exists" at it;
/// - a name to make longer than the 255 UTF-16 units NTFS allows;
/// - whatever the library refuses, the chain rehearsed in full: a name
///   Windows cannot address, a directory inside a metafile, a volume not
///   fit to be written, and one with no room for the chain.
fn missing_directories(
    target: &OsString,
    offset: u64,
    path: &str,
) -> Result<Vec<String>, CliError> {
    let names = path_names(path)?;
    let (mut dev, _) = device::mount(target, offset, false)?;
    let mut at = String::new();
    let mut missing = Vec::new();
    for (i, name) in names.iter().enumerate() {
        at.push('/');
        at.push_str(name);
        if missing.is_empty() {
            match read::resolve_path(&mut dev, &at) {
                Ok(record) => {
                    let (item, enterable) =
                        entry_info(&mut dev, name, record).map_err(|e| ntfs_error(path, e))?;
                    if enterable {
                        continue;
                    }
                    let reparse = match item.get("type") {
                        Some(t) if *t == Json::from("symlink") => Some("a symbolic link"),
                        Some(t) if *t == Json::from("junction") => Some("a junction"),
                        Some(t) if *t == Json::from("dir") => Some("a reparse point"),
                        _ => None,
                    };
                    return Err(CliError::failed(match (i + 1 == names.len(), reparse) {
                        (true, Some(what)) => {
                            format!("{path}: {at} already exists and is {what}, not a directory")
                        }
                        (true, None) => {
                            format!("{path}: {at} already exists and is not a directory")
                        }
                        (false, Some(what)) => format!(
                            "{path}: {at} is {what}, which mkdir --parents does not follow, \
                             so not a directory"
                        ),
                        (false, None) => format!("{path}: {at} is not a directory"),
                    }));
                }
                Err(e) if e.kind() == Kind::NotFound => {}
                Err(e) => return Err(ntfs_error(path, e)),
            }
        }
        let units = name.encode_utf16().count();
        if units > 255 {
            return Err(CliError::failed(format!(
                "{path}: the name {at} is {units} UTF-16 units, too long: NTFS allows 255"
            )));
        }
        missing.push(at.clone());
    }
    if !missing.is_empty() {
        prepared(&mut device::Rehearsal::over(&mut dev), path, |io| {
            make_directories(io, &missing)
        })?;
    }
    Ok(missing)
}

/// Make each of `dirs` in turn, each inside the one before it and the
/// first in a directory already there. If one is still refused once
/// writing has begun, those made before it are removed again, last first,
/// so no part of the chain is left behind.
fn make_directories<D: BlockIo>(dev: &mut D, dirs: &[String]) -> Result<(), Error> {
    for (made, dir) in dirs.iter().enumerate() {
        let (parent, name) = dir
            .rsplit_once('/')
            .expect("a chain's paths are from the root");
        let parent = if parent.is_empty() { "/" } else { parent };
        if let Err(e) = fs_ntfs::write::mkdir_io(dev, parent, name) {
            let e = e.map_message(|m| format!("{dir}: {m}"));
            for undo in dirs[..made].iter().rev() {
                if let Err(u) = fs_ntfs::write::rmdir_io(dev, undo) {
                    return Err(
                        e.map_message(|m| format!("{m}; and removing {undo} again failed: {u}"))
                    );
                }
            }
            // Over a rehearsal nothing was made, so the error says only
            // what was refused.
            return Err(e);
        }
    }
    Ok(())
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

/// `path` split for a rename: the directory part ("/" for a name in the
/// root), the last name with any trailing `/` taken off (empty for the
/// root), and whether there was a trailing `/`. A path not from the root is
/// a usage error, as it is for every other verb.
fn last_name(path: &str) -> Result<(&str, &str, bool), CliError> {
    if !path.starts_with('/') {
        return Err(CliError::usage(format!(
            "{path}: give the path from the root, as /{path}"
        )));
    }
    let trimmed = path.trim_end_matches('/');
    let (parent, name) = trimmed.rsplit_once('/').unwrap_or(("", ""));
    let parent = if parent.is_empty() { "/" } else { parent };
    Ok((parent, name, trimmed.len() < path.len()))
}

/// The directory record at `path`.
fn directory(dev: &mut Device, path: &str) -> Result<u64, Error> {
    let record = read::resolve_path(dev, path)?;
    if !read::read_stat(dev, record)?.is_dir {
        return Err(Error::not_directory(format!("{path} is not a directory")));
    }
    Ok(record)
}

/// Give `source` the name `path`, in the same directory, through the
/// library's rename. Everything that can be refused is refused on a
/// read-only open, so a refused command never opens the image for writing
/// and leaves it byte-identical: first what this verb decides itself, then
/// the rename rehearsed in full, so the library refuses whatever it would
/// refuse on the real volume, in the same words. A rename that would change
/// nothing succeeds without opening the image for writing either.
fn rename(target: &OsString, offset: u64, source: &str, path: &str) -> Result<Outcome, CliError> {
    if let Some((from, name)) = renamable(target, offset, source, path)? {
        edit(target, offset, source, |dev| {
            fs_ntfs::write::rename_io(dev, &from, name)
        })?;
    }
    let report = Json::object([("path", Json::from(path)), ("moved", Json::from(true))]);
    Ok(Outcome::report(report).with_text(format!("moved {source} to {path}")))
}

/// The rename of `source` to `path` -- `source` without a trailing `/`, and
/// the new name -- or `None` when it would change nothing, once every
/// refusal has been ruled out. In order:
///
/// - `.`, `..` or the root as either path;
/// - a directory part of either path that does not resolve, or is not a
///   directory, `source`'s first;
/// - a directory part that is not `source`'s directory: the move this
///   driver cannot make, exit 3;
/// - a missing `source`; a trailing `/` on either path when `source` is not
///   a directory; a new name too long for NTFS;
/// - whatever the library refuses: a name Windows cannot address, a name
///   already there as Windows compares names (through `$UpCase`), a
///   metafile, and a volume not fit to be written.
fn renamable<'p>(
    target: &OsString,
    offset: u64,
    source: &'p str,
    path: &'p str,
) -> Result<Option<(String, &'p str)>, CliError> {
    let (from_parent, from_name, from_slash) = last_name(source)?;
    let (to_parent, to_name, to_slash) = last_name(path)?;
    for (p, name) in [(source, from_name), (path, to_name)] {
        if name.is_empty() || name == "." || name == ".." {
            return Err(CliError::failed(format!(
                "{p}: ., .. and the root cannot be renamed"
            )));
        }
    }
    let (mut dev, _) = device::mount(target, offset, false)?;
    let from_dir = directory(&mut dev, from_parent).map_err(|e| ntfs_error(source, e))?;
    let to_dir = directory(&mut dev, to_parent).map_err(|e| ntfs_error(path, e))?;
    if from_dir != to_dir {
        return Err(CliError::not_implemented(format!(
            "mv across directories: {source} is in {from_parent} and {path} in {to_parent}; \
             this driver renames a name only within its own directory"
        )));
    }
    let from = source.trim_end_matches('/').to_string();
    let record = read::resolve_path(&mut dev, &from).map_err(|e| ntfs_error(source, e))?;
    let is_dir = read::read_stat(&mut dev, record)
        .map_err(|e| ntfs_error(source, e))?
        .is_dir;
    if !is_dir && (from_slash || to_slash) {
        let p = if from_slash { source } else { path };
        return Err(CliError::failed(format!("{p}: not a directory")));
    }
    let units = to_name.encode_utf16().count();
    if units > 255 {
        return Err(CliError::failed(format!(
            "{path}: the name is {units} UTF-16 units, too long: NTFS allows 255"
        )));
    }
    if stored_name(&mut dev, from_dir, record, from_name)
        .map_err(|e| ntfs_error(source, e))?
        .is_some_and(|stored| stored == to_name)
    {
        return Ok(None);
    }
    prepared(&mut device::Rehearsal::over(&mut dev), source, |io| {
        fs_ntfs::write::rename_io(io, &from, to_name)
    })?;
    Ok(Some((from, to_name)))
}

/// The name directory `dir` lists `record` under that Windows takes for
/// `name`: the same name but perhaps in another case.
fn stored_name(
    dev: &mut Device,
    dir: u64,
    record: u64,
    name: &str,
) -> Result<Option<String>, Error> {
    let upcase = fs_ntfs::upcase::UpcaseTable::load_io(dev)?;
    let wanted: Vec<u16> = name.encode_utf16().collect();
    Ok(read::read_dir_entries(dev, dir)?
        .into_iter()
        .filter(|e| e.record_number == record)
        .map(|e| e.name)
        .find(|n| {
            let n: Vec<u16> = n.encode_utf16().collect();
            upcase.cmp_names(&n, &wanted).is_eq()
        }))
}

/// `ln`: give the file at `to` the further name `path` (a hard link), or
/// with `symbolic` make `path` a symbolic link whose target is the text
/// `to`, through the library's `link_io` and `create_symlink_io`. As for
/// `mv`, everything that can be refused is refused on a read-only open:
/// first what this verb decides itself, then the edit rehearsed in full,
/// so the library refuses whatever it would refuse on the real volume, in
/// the same words. The report names the record the new name reaches: the
/// file's own for a hard link, the new link's for a symbolic one.
fn link(
    target: &OsString,
    offset: u64,
    to: &str,
    path: &str,
    symbolic: bool,
) -> Result<Outcome, CliError> {
    let (parent, name) = linkable(target, offset, to, path, symbolic)?;
    let record = edit(target, offset, path, |dev| {
        make_link(dev, to, parent, name, symbolic)
    })?;
    let report = Json::object([
        ("path", Json::from(path)),
        ("linked", Json::from(true)),
        ("inode", Json::from(record)),
    ]);
    Ok(Outcome::report(report).with_text(format!("linked {path}")))
}

/// The link [`link`] makes in `dev`, and the record it reaches.
fn make_link<D: BlockIo>(
    dev: &mut D,
    to: &str,
    parent: &str,
    name: &str,
    symbolic: bool,
) -> Result<u64, Error> {
    if symbolic {
        // The library decides from the target's form whether it is
        // relative, as Windows does; the flag is only a hint.
        return fs_ntfs::write::create_symlink_io(dev, parent, name, to, !to.starts_with('/'));
    }
    let file = hard_link_target(to);
    fs_ntfs::write::link_io(dev, file, parent, name)?;
    read::resolve_path(dev, file)
}

/// A hard link's target without any trailing `/`; the root stays `/`.
fn hard_link_target(to: &str) -> &str {
    match to.trim_end_matches('/') {
        "" => "/",
        file => file,
    }
}

/// The directory and last name of [`link`]'s `path`, once every refusal
/// has been ruled out. In order:
///
/// - `.`, `..` or the root as `path`, and a trailing `/` on it: a link is
///   never a directory;
/// - for a hard link, a `to` not from the root (a usage error), a missing
///   one, and a trailing `/` on one that is not a directory;
/// - whatever the library refuses: a missing directory, a name Windows
///   cannot address, a name already there as Windows compares names
///   (through `$UpCase`), a hard link to a directory, a file that already
///   has the 1024 names Windows allows, a record with no room for another
///   name, a metafile, an empty symbolic link target, and a volume not fit
///   to be written.
fn linkable<'p>(
    target: &OsString,
    offset: u64,
    to: &str,
    path: &'p str,
    symbolic: bool,
) -> Result<(&'p str, &'p str), CliError> {
    let (parent, name, slash) = last_name(path)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err(CliError::failed(format!(
            "{path}: ., .. and the root cannot be made a link"
        )));
    }
    if slash {
        return Err(CliError::failed(format!(
            "{path}: not a directory; a link is never one"
        )));
    }
    if !symbolic {
        last_name(to)?;
    }
    let (mut dev, _) = device::mount(target, offset, false)?;
    if !symbolic {
        let file = hard_link_target(to);
        let record = read::resolve_path(&mut dev, file).map_err(|e| ntfs_error(to, e))?;
        let is_dir = read::read_stat(&mut dev, record)
            .map_err(|e| ntfs_error(to, e))?
            .is_dir;
        if !is_dir && file.len() < to.len() {
            return Err(CliError::failed(format!("{to}: not a directory")));
        }
    }
    prepared(&mut device::Rehearsal::over(&mut dev), path, |io| {
        make_link(io, to, parent, name, symbolic)
    })?;
    Ok((parent, name))
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
