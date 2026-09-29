//! `rust-ntfs format` — build a fresh NTFS volume.
//!
//! Wraps `fs_ntfs::mkfs::format_filesystem`, the same entry point the C
//! ABI exposes to an embedding filesystem extension — so formatting a
//! removable disk through a host application and formatting an image
//! from this CLI exercise the same code path.
//!
//! SHARED WITH `mkfs.ntfs`. The command-line tools' binary includes this
//! file through `#[path]` and parses its own command line with clap, then
//! hands the same [`Opts`] to [`execute`]: one set of defaults, one set of
//! checks and one set of messages, whichever front end parsed the flags.
//! This hand parser stays because `rust-ntfs` is the test matrix's driver,
//! built without the `cli` feature, and its accepted flags must not move.
//!
//! Convention: the device/file MUST already exist at the target size,
//! same as every other mkfs.* tool. Use `truncate -s 256M out.img`
//! (Linux/macOS) or `fsutil file createnew out.img 268435456`
//! (Windows). The `--create-size <SIZE>` flag collapses that to a
//! single command for image-file workflows.

use fs_ntfs::block_io::PathIo;
use fs_ntfs::mkfs::format_filesystem;
use std::path::Path;
use std::process::ExitCode;

/// Usage text. `{PROG}` stands in for however the tool was invoked --
/// two binaries share this file, `rust-ntfs format` and `mkfs.ntfs`,
/// and a message naming the wrong one sends the reader to a command
/// they did not run.
const USAGE: &str = "\
Usage: {PROG} [options] <device>

Options:
  -L, --label <label>      Volume label (max 32 UTF-16 code units after encode).
  -c, --cluster-size <n>   Cluster size in bytes. Power of 2, 512..=65536.
                           Default: 4096.
  --mft-record-size <n>    MFT record size in bytes. Power of 2, 2048..=16384.
                           512 cannot hold $Secure; 1024 cannot hold the
                           populated root metadata. Default: 4096.
  --serial <hex>           NTFS volume serial number (16 hex chars). Default:
                           random.
  -Q, --quick              Quick format. Accepted; the on-disk layout we
                           write is always quick-format-equivalent (no
                           full-volume zero pass).
  -f, --fast               Fast format alias for -Q.
  -F, --force              Format even if device looks in use. (Accepted.)
  -n                       Dry-run: parse args + open device but do not write.
  -q, --quiet              Suppress non-error output.
  --create-size <SIZE>     If device doesn't exist, create it as a regular
                           file of the given size first. SIZE accepts K/M/G/T
                           suffixes (1024-based). Refuses to apply to existing
                           block devices — only valid for image files.
  -V, --version            Print the version and exit.
  -h, --help               Print this help and exit.

Positional:
  device                   Path to a block device or pre-sized regular file.
";

/// The cluster size used when none is given.
pub const DEFAULT_CLUSTER_SIZE: u32 = 4096;
/// The MFT record size used when none is given.
pub const DEFAULT_MFT_RECORD_SIZE: u32 = 4096;
/// `$VOLUME_NAME` holds 64 bytes: 32 UTF-16 code units.
pub const MAX_LABEL_UNITS: usize = 32;

/// Everything a command line can ask of the formatter.
#[derive(Default, Debug)]
pub struct Opts {
    pub label: Option<String>,
    pub cluster_size: Option<u32>,
    pub mft_record_size: Option<u32>,
    pub serial: Option<u64>,
    pub force: bool,
    pub quick: bool,
    pub dry_run: bool,
    pub quiet: bool,
    pub create_size: Option<u64>,
    pub device: Option<String>,
}

/// What [`execute`] did. `rust-ntfs format` reports nothing on stdout and
/// reads none of it; `mkfs.ntfs` turns it into its JSON report.
#[allow(dead_code)]
pub struct Done {
    /// The target's size in bytes: for a dry run whose target does not
    /// exist yet, the size `--create-size` would have made it.
    pub device_bytes: Option<u64>,
    /// Whether anything was written: false for a dry run.
    pub formatted: bool,
    pub cluster_size: u32,
    pub mft_record_size: u32,
}

/// `prog` is the name this invocation should call itself by, in usage
/// and in every message.
fn usage(prog: &str) -> String {
    USAGE.replace("{PROG}", prog)
}

pub fn run(args: Vec<String>, prog: &str) -> ExitCode {
    match run_inner(args, prog) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{prog}: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: Vec<String>, prog: &str) -> Result<(), String> {
    let opts = parse_args(args, prog)?;
    let device = opts
        .device
        .as_deref()
        .ok_or_else(|| format!("missing positional <device> argument\n\n{}", usage(prog)))?;
    execute(&opts, device, prog).map(|_| ())
}

/// Format `device` as `opts` asks, calling itself `prog` in every message.
/// Progress goes to stderr unless `opts.quiet`.
pub fn execute(opts: &Opts, device: &str, prog: &str) -> Result<Done, String> {
    let cluster_size = opts.cluster_size.unwrap_or(DEFAULT_CLUSTER_SIZE);
    let mft_record_size = opts.mft_record_size.unwrap_or(DEFAULT_MFT_RECORD_SIZE);

    // THE CAP THE USAGE TEXT PROMISES, CHECKED BEFORE ANYTHING IS
    // TOUCHED. `-L` documents "max 32 UTF-16 code units after encode"
    // and nothing enforced it (#180): a longer label either shipped an
    // out-of-spec volume or failed deep inside the formatter, after the
    // device had already been partly written. Refusing here costs the
    // user a retyped command instead of a half-formatted disk.
    //
    // Counted in UTF-16 code units, not characters: the cap is on what
    // goes in `$VOLUME_NAME`, so an emoji is two and an accented letter
    // may be one or two depending on how it is composed.
    if let Some(label) = opts.label.as_deref() {
        check_label(label)?;
    }

    if let Some(n) = opts.create_size {
        // -n IS A DRY RUN OF THE WHOLE COMMAND, not of the format step.
        // `--create-size` used to run unconditionally, so `format -n
        // --create-size 64M /tmp/new.img` created and sized a 64 MiB file
        // and then said "no writes performed" (#181). Creating a file is
        // a write, and the sentence was false about the only thing the
        // command had done.
        if opts.dry_run {
            if !opts.quiet {
                match std::fs::metadata(device) {
                    Ok(meta) => eprintln!(
                        "{prog}: dry-run — would leave existing {device} as-is ({} bytes)",
                        meta.len()
                    ),
                    Err(_) => eprintln!("{prog}: dry-run — would create {device} ({n} bytes)"),
                }
            }
            return Ok(Done {
                // The size the target has, or would have been created at.
                device_bytes: Some(std::fs::metadata(device).map_or(n, |m| m.len())),
                formatted: false,
                cluster_size,
                mft_record_size,
            });
        }
        match std::fs::metadata(device) {
            Ok(meta) => {
                let ft = meta.file_type();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if ft.is_block_device() || ft.is_char_device() {
                        return Err(format!(
                            "--create-size refuses to apply to {device}: looks like a real block/char device, \
                             not a regular file."
                        ));
                    }
                }
                if !ft.is_file() {
                    return Err(format!(
                        "--create-size: {device} exists but is not a regular file"
                    ));
                }
                if !opts.quiet {
                    eprintln!(
                        "{prog}: --create-size: {device} already exists ({} bytes); leaving as-is",
                        meta.len()
                    );
                }
            }
            Err(_) => {
                let f = std::fs::File::create(device)
                    .map_err(|e| format!("--create-size: create {device}: {e}"))?;
                f.set_len(n)
                    .map_err(|e| format!("--create-size: set_len({n}) on {device}: {e}"))?;
                drop(f);
                if !opts.quiet {
                    eprintln!("{prog}: --create-size: created {device} ({n} bytes)");
                }
            }
        }
    }

    let mut dev =
        PathIo::open_rw(Path::new(device)).map_err(|e| format!("open {device} read-write: {e}"))?;

    let size = {
        use fs_ntfs::block_io::BlockIo;
        dev.size()
    };
    if size == 0 {
        return Err(format!(
            "device {device} reports size 0 — pre-create with truncate / fsutil first"
        ));
    }

    if !opts.quiet {
        eprintln!(
            "{prog}: formatting {device} ({size} bytes, cluster_size={cluster_size}, mft_record_size={mft_record_size}{}{})",
            if opts.quick { ", quick" } else { "" },
            if opts.dry_run { ", dry-run" } else { "" }
        );
    }

    if opts.dry_run {
        if !opts.quiet {
            eprintln!("{prog}: dry-run — no writes performed");
        }
        let _ = (opts.force, opts.quick);
        return Ok(Done {
            device_bytes: Some(size),
            formatted: false,
            cluster_size,
            mft_record_size,
        });
    }

    format_filesystem(
        &mut dev,
        size,
        cluster_size,
        mft_record_size,
        opts.label.as_deref(),
        opts.serial,
    )
    .map_err(|e| format!("format failed: {e}"))?;

    {
        use fs_ntfs::block_io::BlockIo;
        dev.sync().map_err(|e| format!("fsync failed: {e}"))?;
    }

    if !opts.quiet {
        eprintln!("{prog}: {device} formatted successfully");
    }
    Ok(Done {
        device_bytes: Some(size),
        formatted: true,
        cluster_size,
        mft_record_size,
    })
}

/// THE CAP THE USAGE TEXT PROMISES: at most 32 UTF-16 code units.
pub fn check_label(label: &str) -> Result<(), String> {
    let units = label.encode_utf16().count();
    if units > MAX_LABEL_UNITS {
        return Err(format!(
            "--label: {units} UTF-16 code units, and the limit is {MAX_LABEL_UNITS}. NTFS stores \
             the label in $VOLUME_NAME, which is 64 bytes. Shorten it: {label:?}"
        ));
    }
    Ok(())
}

fn parse_args(args: Vec<String>, prog: &str) -> Result<Opts, String> {
    let mut opts = Opts::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", usage(prog));
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!(
                    "{prog} ({}) {}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            "-L" | "--label" => {
                opts.label = Some(
                    iter.next()
                        .ok_or_else(|| format!("{arg} requires a label argument"))?,
                );
            }
            "-c" | "--cluster-size" => {
                let v = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a cluster size argument"))?;
                let n: u32 = v
                    .parse()
                    .map_err(|_| format!("{arg}: not a valid number: {v}"))?;
                opts.cluster_size = Some(n);
            }
            "--mft-record-size" => {
                let v = iter
                    .next()
                    .ok_or_else(|| "--mft-record-size requires a value".to_string())?;
                let n: u32 = v
                    .parse()
                    .map_err(|_| format!("--mft-record-size: not a valid number: {v}"))?;
                opts.mft_record_size = Some(check_mft_record_size(n)?);
            }
            "--serial" => {
                let v = iter
                    .next()
                    .ok_or_else(|| "--serial requires a hex value".to_string())?;
                opts.serial = Some(parse_hex_u64(&v)?);
            }
            "-Q" | "--quick" | "-f" | "--fast" => opts.quick = true,
            "-F" | "--force" => opts.force = true,
            "-n" => opts.dry_run = true,
            "-q" | "--quiet" => opts.quiet = true,
            "--create-size" => {
                let v = iter.next().ok_or_else(|| {
                    "--create-size requires a SIZE argument (e.g. 256M)".to_string()
                })?;
                opts.create_size = Some(parse_size(&v)?);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag: {other}\n\n{}", usage(prog)));
            }
            _ => {
                if opts.device.is_some() {
                    return Err(format!(
                        "extra positional argument: {arg} (only one device may be given)"
                    ));
                }
                opts.device = Some(arg);
            }
        }
    }

    Ok(opts)
}

/// The MFT record sizes the formatter cannot build are refused while the
/// command line is read, before anything is created or opened.
pub fn check_mft_record_size(n: u32) -> Result<u32, String> {
    // The formatter's NTFS 3.1 `$Secure` record must contain
    // `$STANDARD_INFORMATION`, `$FILE_NAME`, non-resident `$SDS`,
    // and the resident `$SDH` / `$SII` view indexes. That layout
    // is 120 bytes too large at 512 bytes. At 1024 bytes the
    // populated root directory metadata is the next system record
    // that cannot fit. Supporting either size would require a
    // different, Windows-validated attribute-list layout;
    // accepting the values and failing while formatting is not
    // support. Reject them while parsing, before `--create-size`
    // can create an image or the target is opened read-write.
    let unsupported_reason = match n {
        512 => Some(
            "the mandatory $Secure metadata ($SDS, $SDH, and $SII) does not fit in \
             a 512-byte MFT record",
        ),
        1024 => Some(
            "the mandatory populated root directory metadata does not fit in a \
             1024-byte MFT record",
        ),
        _ => None,
    };
    if let Some(reason) = unsupported_reason {
        return Err(format!(
            "--mft-record-size {n} is unsupported: {reason}; the smallest supported \
             size is 2048 bytes"
        ));
    }
    Ok(n)
}

pub fn parse_size(s: &str) -> Result<u64, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("--create-size: empty size argument".to_string());
    }
    let s = trimmed.strip_suffix(['B', 'b']).unwrap_or(trimmed);
    let (num, mult): (&str, u64) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 1024),
        Some('M' | 'm') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G' | 'g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        Some('T' | 't') => (&s[..s.len() - 1], 1024 * 1024 * 1024 * 1024),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => return Err(format!("--create-size: unrecognised size suffix in {s:?}")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("--create-size: not a valid number: {num:?}"))?;
    n.checked_mul(mult)
        .ok_or_else(|| format!("--create-size: {s} overflows u64"))
}

pub fn parse_hex_u64(s: &str) -> Result<u64, String> {
    let cleaned = s.trim_start_matches("0x").trim_start_matches("0X");
    if cleaned.is_empty() || cleaned.len() > 16 {
        return Err(format!(
            "serial must be 1..=16 hex chars (with optional 0x prefix), got {} chars",
            cleaned.len()
        ));
    }
    u64::from_str_radix(cleaned, 16).map_err(|_| format!("serial has non-hex character: {s}"))
}
