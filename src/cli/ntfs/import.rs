//! Single-file import over core 0.3.14's bounded writer destination.
//!
//! The file is written into a staging name beside the destination, in the
//! shared pump's fixed chunks at increasing offsets, synced, optionally read
//! back through a fresh open, and only then renamed to its final name. The
//! driver's rename refuses a name already there, so `--replace` is refused
//! with exit 3 rather than deleting the old file first.
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use fs_core::cli::{transfer, CliError, Json, Outcome};
use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::error::Kind;
use fs_ntfs::read;

use super::device::{self, Device};
use super::fs::{ntfs_error, prepared, split_path};

fn error(e: impl std::fmt::Display) -> CliError {
    CliError::failed(e.to_string())
}

struct LocalSource {
    file: File,
    size: u64,
    temporary: Option<PathBuf>,
}

impl LocalSource {
    fn open(path: &OsStr) -> Result<Self, CliError> {
        if !std::fs::symlink_metadata(path).map_err(error)?.is_file() {
            return Err(error(
                "import source must be a regular file; symlinks are not followed",
            ));
        }
        let file = File::open(path).map_err(error)?;
        let size = file.metadata().map_err(error)?.len();
        Ok(Self {
            file,
            size,
            temporary: None,
        })
    }

    fn stdin() -> Result<Self, CliError> {
        // The released shared pump requires a known length. Spool to disk,
        // retaining bounded memory, and remove the private file on every return.
        for n in 0..1000 {
            let path = std::env::temp_dir()
                .join(format!("ntfs-import-{}-{n}.partial", std::process::id()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = match options.open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error(e)),
            };
            let mut source = Self {
                file,
                size: 0,
                temporary: Some(path),
            };
            source.size =
                std::io::copy(&mut std::io::stdin().lock(), &mut source.file).map_err(error)?;
            source.file.seek(SeekFrom::Start(0)).map_err(error)?;
            return Ok(source);
        }
        Err(error("no free stdin spool name"))
    }
}

impl Drop for LocalSource {
    fn drop(&mut self) {
        if let Some(path) = &self.temporary {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl transfer::FileSource for LocalSource {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, CliError> {
        self.file.seek(SeekFrom::Start(offset)).map_err(error)?;
        self.file.read(buf).map_err(error)
    }
}

/// A staged file with a recognisable name in the destination's directory.
/// Persistence syncs and closes the writable open before verification or
/// publication.
struct Stage {
    target: OsString,
    offset: u64,
    staged: String,
    destination: String,
    dev: Option<Device>,
    written: u64,
    published: bool,
}

impl Stage {
    fn create(target: &OsStr, offset: u64, path: &str, replace: bool) -> Result<Self, CliError> {
        if path.ends_with('/') {
            return Err(error(
                "import PATH must name a file, without a trailing slash",
            ));
        }
        let (parent, destination) = split_path(path)?;
        let units = destination.encode_utf16().count();
        if units > 255 {
            return Err(error(format!(
                "{path}: the name is {units} UTF-16 units, too long: NTFS allows 255"
            )));
        }
        let target = target.to_os_string();
        let (mut dev, _) = device::mount(&target, offset, false)?;
        let dir = read::resolve_path(&mut dev, &parent).map_err(|e| ntfs_error(&parent, e))?;
        if !read::read_stat(&mut dev, dir)
            .map_err(|e| ntfs_error(&parent, e))?
            .is_dir
        {
            return Err(error(format!("{parent}: not a directory")));
        }
        match read::resolve_path(&mut dev, path) {
            Ok(_) if replace => {
                return Err(CliError::not_implemented(format!(
                    "{path}: --replace needs a rename that replaces the old name in one \
                     step; this driver's rename refuses a name already there"
                )))
            }
            Ok(_) => return Err(error(format!("{path}: destination exists"))),
            Err(e) if e.kind() == Kind::NotFound => {}
            Err(e) => return Err(ntfs_error(path, e)),
        }
        let join = |name: &str| {
            if parent == "/" {
                format!("/{name}")
            } else {
                format!("{parent}/{name}")
            }
        };
        let mut chosen = None;
        for n in 0..1000 {
            let name = format!(".dj-import-{}-{n}.partial", std::process::id());
            match read::resolve_path(&mut dev, &join(&name)) {
                Err(e) if e.kind() == Kind::NotFound => {
                    chosen = Some(name);
                    break;
                }
                Ok(_) => continue,
                Err(e) => return Err(ntfs_error(path, e)),
            }
        }
        let name = chosen.ok_or_else(|| error("no free import staging name"))?;
        drop(dev);
        let (mut dev, _) = device::mount(&target, offset, true)?;
        prepared(&mut dev, path, |dev| {
            fs_ntfs::write::create_file_io(dev, &parent, &name)
        })?;
        Ok(Self {
            target,
            offset,
            staged: join(&name),
            destination: destination.to_string(),
            dev: Some(dev),
            written: 0,
            published: false,
        })
    }

    fn copy(
        &mut self,
        source: &mut dyn transfer::FileSource,
    ) -> Result<transfer::Copied, CliError> {
        let copied = transfer::export_file(source, transfer::Destination::Writer(self))?;
        let mut dev = self.dev.take().expect("live staging open");
        BlockIo::sync(&mut dev).map_err(|e| ntfs_error(&self.staged, e.into()))?;
        Ok(copied)
    }

    fn verify(&self, copied: &transfer::Copied) -> Result<(), CliError> {
        let (mut dev, _) = device::mount(&self.target, self.offset, false)?;
        let record =
            read::resolve_path(&mut dev, &self.staged).map_err(|e| ntfs_error(&self.staged, e))?;
        let size = read::read_stat(&mut dev, record)
            .map_err(|e| ntfs_error(&self.staged, e))?
            .size;
        let mut source = StagedSource {
            dev: &mut dev,
            record,
            size,
            path: &self.staged,
        };
        let readback = transfer::export_file(
            &mut source,
            transfer::Destination::Writer(&mut std::io::sink()),
        )?;
        if readback.bytes != copied.bytes || readback.sha256 != copied.sha256 {
            return Err(error("staged import verification failed"));
        }
        Ok(())
    }

    fn publish(&mut self) -> Result<(), CliError> {
        let (mut dev, _) = device::mount(&self.target, self.offset, true)?;
        // Same-directory rename of a name that does not exist yet: the
        // staged file's $FILE_NAME and its directory index entry are
        // rewritten together, so the final name appears complete or not at all.
        prepared(&mut dev, &self.staged, |dev| {
            fs_ntfs::write::rename_io(dev, &self.staged, &self.destination)
        })?;
        self.published = true;
        BlockIo::sync(&mut dev)
            .map_err(|e| error(format!("import published, but syncing failed: {e}")))
    }

    fn abort(&mut self) -> Result<(), CliError> {
        self.dev.take(); // retire an open after any write failure
        let (mut dev, _) = device::mount(&self.target, self.offset, true)?;
        match prepared(
            &mut dev,
            &self.staged,
            |dev| match fs_ntfs::write::unlink_io(dev, &self.staged) {
                Err(e) if e.kind() == Kind::NotFound => Ok(()),
                other => other,
            },
        ) {
            Ok(()) => {}
            Err(e) => return Err(e),
        }
        BlockIo::sync(&mut dev).map_err(|e| ntfs_error(&self.staged, e.into()))?;
        self.published = true; // cleanup is complete
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.abort();
        }
    }
}

impl Write for Stage {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let dev = self.dev.as_mut().expect("live staging open");
        fs_ntfs::write::write_at_io(dev, &self.staged, self.written, buf)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let dev = self.dev.as_mut().expect("live staging open");
        BlockIo::sync(dev).map_err(|e| std::io::Error::other(e.to_string()))
    }
}

struct StagedSource<'a> {
    dev: &'a mut Device,
    record: u64,
    size: u64,
    path: &'a str,
}

impl transfer::FileSource for StagedSource<'_> {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, CliError> {
        let bytes = read::read_attribute_range(
            self.dev,
            self.record,
            AttrType::Data,
            None,
            offset,
            buf.len(),
        )
        .map_err(|e| ntfs_error(self.path, e))?;
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

/// Whether `source` is the same host file as the backing image.
#[cfg(unix)]
fn aliases(source: &File, target: &OsStr) -> Result<bool, CliError> {
    use std::os::unix::fs::MetadataExt;
    let a = source.metadata().map_err(error)?;
    let b = std::fs::metadata(target).map_err(error)?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

#[cfg(not(unix))]
fn aliases(_source: &File, _target: &OsStr) -> Result<bool, CliError> {
    Ok(false)
}

pub(super) fn run(
    target: &OsString,
    offset: u64,
    path: &str,
    src: &OsString,
    replace: bool,
    verify: bool,
) -> Result<Outcome, CliError> {
    let mut source = if src == "-" {
        None
    } else {
        Some(LocalSource::open(src)?)
    };
    if let Some(source) = &source {
        if aliases(&source.file, target)? {
            return Err(error("import source aliases the backing image"));
        }
    }
    let mut stage = Stage::create(target, offset, path, replace)?;
    let result: Result<Outcome, CliError> = (|| {
        if source.is_none() {
            source = Some(LocalSource::stdin()?);
        }
        let copied = stage.copy(source.as_mut().expect("opened source"))?;
        if verify {
            stage.verify(&copied)?;
        }
        stage.publish()?;
        let text = format!(
            "imported {} bytes to {path} (sha256 {})",
            copied.bytes,
            copied.sha256_hex()
        );
        Ok(Outcome::report(Json::object([
            ("path", Json::from(path)),
            ("bytes", Json::from(copied.bytes)),
            ("sha256", Json::from(copied.sha256_hex())),
            ("replaced", Json::from(false)),
            ("verified", Json::from(verify)),
        ]))
        .with_text(text))
    })();
    if result.is_err() && !stage.published {
        if let Err(cleanup) = stage.abort() {
            return Err(error(format!(
                "{}; cleanup failed: {}; staging name {} remains",
                result.expect_err("failed import").message,
                cleanup.message,
                stage.staged
            )));
        }
    }
    result
}
