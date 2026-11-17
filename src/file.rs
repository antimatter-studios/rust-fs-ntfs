use crate::attr_io::AttrType;
use crate::error::Error;
use crate::mft_io::{self, MFT_FLAG_IN_USE};
use crate::read::{self, DirEntry, Stat};
use crate::volume::{Volume, VolumeAPI};
use fs_core::BlockRead;

#[derive(Clone, Debug)]
pub struct FileHandle {
    record_number: u64,
    sequence: u16,
    volume_id: u64,
}

impl FileHandle {
    pub fn record_number(&self) -> u64 {
        self.record_number
    }
}

pub trait FileAPI {
    fn read_directory(&self, path: &str) -> Result<Vec<DirEntry>, Error>;
    fn metadata(&self, path: &str) -> Result<Stat, Error>;
    fn open_file(&self, path: &str) -> Result<FileHandle, Error>;
    fn read_file_at(
        &self,
        file: &FileHandle,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error>;
    fn read_stream_at(
        &self,
        path: &str,
        name: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, Error>;
    fn create_file(&self, parent: &str, name: &str) -> Result<u64, Error>;
    fn create_directory(&self, parent: &str, name: &str) -> Result<u64, Error>;
    fn write_file_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<u64, Error>;
    fn set_len(&self, path: &str, length: u64) -> Result<u64, Error>;
    fn rename(&self, path: &str, new_name: &str) -> Result<(), Error>;
    fn remove_file(&self, path: &str) -> Result<(), Error>;
    fn remove_directory(&self, path: &str) -> Result<(), Error>;
}

impl<Device: BlockRead> Volume<Device> {
    fn writable(&self) -> Result<(), Error> {
        if !self.is_writable() {
            return Err(Error::refused("volume is read-only"));
        }
        Ok(())
    }
}

impl<Device: BlockRead> FileAPI for Volume<Device> {
    fn read_directory(&self, path: &str) -> Result<Vec<DirEntry>, Error> {
        self.raw.with_io(|io| {
            let record = read::resolve_path(io, path)?;
            read::read_dir_entries(io, record)
        })
    }

    fn metadata(&self, path: &str) -> Result<Stat, Error> {
        self.raw.with_io(|io| {
            let record = read::resolve_path(io, path)?;
            read::read_stat(io, record)
        })
    }

    fn open_file(&self, path: &str) -> Result<FileHandle, Error> {
        self.raw.with_io(|io| {
            let record_number = read::resolve_path(io, path)?;
            if read::read_stat(io, record_number)?.is_dir {
                return Err(Error::is_directory("cannot open a directory as a file"));
            }
            let (_, record) = mft_io::read_mft_record_io(io, record_number)?;
            if mft_io::record_flags(&record) & MFT_FLAG_IN_USE == 0 {
                return Err(Error::not_found("file record is not in use"));
            }
            Ok(FileHandle {
                record_number,
                sequence: mft_io::record_sequence(&record),
                volume_id: self.id,
            })
        })
    }

    fn read_file_at(
        &self,
        file: &FileHandle,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        if file.volume_id != self.id {
            return Err(Error::invalid("file handle belongs to another volume"));
        }
        self.raw.with_io(|io| {
            let (_, record) = mft_io::read_mft_record_io(io, file.record_number)?;
            if mft_io::record_sequence(&record) != file.sequence
                || mft_io::record_flags(&record) & MFT_FLAG_IN_USE == 0
            {
                return Err(Error::not_found("file handle is stale"));
            }
            let data = read::read_attribute_range(
                io,
                file.record_number,
                AttrType::Data,
                None,
                offset,
                output.len(),
            )?;
            output[..data.len()].copy_from_slice(&data);
            Ok(data.len())
        })
    }

    fn read_stream_at(
        &self,
        path: &str,
        name: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, Error> {
        self.raw.with_io(|io| {
            let record = read::resolve_path(io, path)?;
            read::read_attribute_range(
                io,
                record,
                AttrType::Data,
                if name.is_empty() { None } else { Some(name) },
                offset,
                length,
            )
        })
    }

    fn create_file(&self, parent: &str, name: &str) -> Result<u64, Error> {
        self.writable()?;
        self.raw
            .with_io(|io| crate::write::create_file_io(io, parent, name))
    }
    fn create_directory(&self, parent: &str, name: &str) -> Result<u64, Error> {
        self.writable()?;
        self.raw
            .with_io(|io| crate::write::mkdir_io(io, parent, name))
    }
    fn write_file_at(&self, path: &str, offset: u64, data: &[u8]) -> Result<u64, Error> {
        self.writable()?;
        self.raw
            .with_io(|io| crate::write::write_at_io(io, path, offset, data))
    }
    fn set_len(&self, path: &str, length: u64) -> Result<u64, Error> {
        self.writable()?;
        self.raw
            .with_io(|io| crate::write::truncate_io(io, path, length))
    }
    fn rename(&self, path: &str, new_name: &str) -> Result<(), Error> {
        self.writable()?;
        self.raw
            .with_io(|io| crate::write::rename_io(io, path, new_name))
    }
    fn remove_file(&self, path: &str) -> Result<(), Error> {
        self.writable()?;
        self.raw.with_io(|io| crate::write::unlink_io(io, path))
    }
    fn remove_directory(&self, path: &str) -> Result<(), Error> {
        self.writable()?;
        self.raw.with_io(|io| crate::write::rmdir_io(io, path))
    }
}
