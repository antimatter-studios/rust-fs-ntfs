use crate::block_io::BlockIo;
use crate::error::Error;
use crate::mft_io::{self, BootParams, NTFS_OEM_ID};
use fs_core::{BlockDevice, BlockRead};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static NEXT_VOLUME: AtomicU64 = AtomicU64::new(1);

type WriteAt<Device> = fn(&Device, u64, &[u8]) -> Result<(), Error>;
type Flush<Device> = fn(&Device) -> Result<(), Error>;

pub trait NtfsVolumeAPI {
    fn size_bytes(&self) -> u64;
    fn geometry(&self) -> Result<BootParams, Error>;
    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), Error>;
    fn write_volume_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Error>;
    fn is_writable(&self) -> bool;
    fn flush(&self) -> Result<(), Error>;
}

pub struct RawVolume<Device: BlockRead> {
    device: Device,
    writer: Option<WriteAt<Device>>,
    flusher: Option<Flush<Device>>,
    operations: Mutex<()>,
}

impl<Device: BlockRead> RawVolume<Device> {
    pub fn open(device: Device) -> Self {
        Self {
            device,
            writer: None,
            flusher: None,
            operations: Mutex::new(()),
        }
    }

    /// The device the volume reads.
    pub(crate) fn device(&self) -> &Device {
        &self.device
    }

    pub(crate) fn with_io<T>(
        &self,
        operation: impl FnOnce(&mut Transport<'_, Device>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let _guard = self
            .operations
            .lock()
            .map_err(|_| Error::io("volume lock poisoned"))?;
        operation(&mut Transport { volume: self })
    }

    fn check_range(&self, offset: u64, length: usize) -> Result<(), Error> {
        offset
            .checked_add(length as u64)
            .filter(|end| *end <= NtfsVolumeAPI::size_bytes(self))
            .map(|_| ())
            .ok_or_else(|| Error::invalid("volume byte range exceeds source"))
    }
}

impl<Device: BlockDevice> RawVolume<Device> {
    pub fn open_writable(device: Device) -> Result<Self, Error> {
        if !device.is_writable() {
            return Err(Error::refused("device is read-only"));
        }
        Ok(Self {
            device,
            writer: Some(|device, offset, data| {
                device
                    .write_at(offset, data)
                    .map_err(|error| Error::io(error.to_string()))
            }),
            flusher: Some(|device| device.flush().map_err(|error| Error::io(error.to_string()))),
            operations: Mutex::new(()),
        })
    }
}

impl<Device: BlockRead> NtfsVolumeAPI for RawVolume<Device> {
    fn size_bytes(&self) -> u64 {
        self.device.size_bytes()
    }

    fn geometry(&self) -> Result<BootParams, Error> {
        let boot = self.with_io(|io| mft_io::read_boot_params_io(io))?;
        if &boot.oem_id != NTFS_OEM_ID || boot.total_sectors == 0 {
            return Err(Error::invalid("invalid NTFS volume identity or length"));
        }
        Ok(boot)
    }

    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), Error> {
        self.check_range(offset, output.len())?;
        self.device
            .read_at(offset, output)
            .map_err(|error| Error::io(error.to_string()))
    }

    fn write_volume_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        let writer = self
            .writer
            .ok_or_else(|| Error::refused("volume is read-only"))?;
        self.check_range(offset, data.len())?;
        writer(&self.device, offset, data)
    }

    fn is_writable(&self) -> bool {
        self.writer.is_some()
    }

    fn flush(&self) -> Result<(), Error> {
        match self.flusher {
            Some(flush) => flush(&self.device),
            None => Ok(()),
        }
    }
}

pub(crate) struct Transport<'volume, Device: BlockRead> {
    volume: &'volume RawVolume<Device>,
}

impl<Device: BlockRead> BlockIo for Transport<'_, Device> {
    fn read_exact_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), String> {
        NtfsVolumeAPI::read_volume_bytes(self.volume, offset, output).map_err(Into::into)
    }
    fn write_all_at(&mut self, offset: u64, data: &[u8]) -> Result<(), String> {
        NtfsVolumeAPI::write_volume_bytes(self.volume, offset, data).map_err(Into::into)
    }
    fn size(&self) -> u64 {
        NtfsVolumeAPI::size_bytes(self.volume)
    }
    fn sync(&mut self) -> Result<(), String> {
        NtfsVolumeAPI::flush(self.volume).map_err(Into::into)
    }
    fn can_sync(&self) -> bool {
        NtfsVolumeAPI::is_writable(self.volume)
    }
}

pub struct Volume<Device: BlockRead> {
    pub(crate) raw: RawVolume<Device>,
    boot: BootParams,
    pub(crate) id: u64,
}

impl<Device: BlockRead> Volume<Device> {
    pub fn open(device: Device) -> Result<Self, Error> {
        let volume = Self::open_for_salvage(device)?;
        volume
            .raw
            .with_io(|io| crate::read::read_volume_info(io).map(|_| ()))?;
        Ok(volume)
    }

    pub fn open_for_salvage(device: Device) -> Result<Self, Error> {
        Self::from_raw(RawVolume::open(device))
    }

    /// A volume read with boot parameters the caller already validated,
    /// such as the backup boot sector's.
    pub(crate) fn with_boot(raw: RawVolume<Device>, boot: BootParams) -> Self {
        Self {
            raw,
            boot,
            id: NEXT_VOLUME.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn from_raw(raw: RawVolume<Device>) -> Result<Self, Error> {
        let boot = NtfsVolumeAPI::geometry(&raw)?;
        Ok(Self {
            raw,
            boot,
            id: NEXT_VOLUME.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn boot(&self) -> &BootParams {
        &self.boot
    }

    pub fn raw(&self) -> &RawVolume<Device> {
        &self.raw
    }

    pub fn cluster_offset(&self, cluster: u64) -> Result<u64, Error> {
        cluster
            .checked_mul(self.boot.cluster_size)
            .filter(|offset| *offset < NtfsVolumeAPI::size_bytes(self))
            .ok_or_else(|| Error::invalid("cluster outside NTFS volume"))
    }

    pub fn volume_info(&self) -> Result<crate::read::VolumeInfo, Error> {
        self.raw.with_io(|io| crate::read::read_volume_info(io))
    }
}

impl<Device: BlockDevice> Volume<Device> {
    pub fn open_writable(device: Device) -> Result<Self, Error> {
        let volume = Self::from_raw(RawVolume::open_writable(device)?)?;
        volume.raw.with_io(|io| {
            crate::read::read_volume_info(io)?;
            crate::prepare_rw_mount(io, false)
        })?;
        Ok(volume)
    }
}

impl<Device: BlockRead> NtfsVolumeAPI for Volume<Device> {
    fn size_bytes(&self) -> u64 {
        self.boot
            .volume_bytes()
            .min(NtfsVolumeAPI::size_bytes(&self.raw))
    }
    fn geometry(&self) -> Result<BootParams, Error> {
        Ok(self.boot)
    }
    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), Error> {
        offset
            .checked_add(output.len() as u64)
            .filter(|end| *end <= NtfsVolumeAPI::size_bytes(self))
            .ok_or_else(|| Error::invalid("read exceeds declared NTFS volume"))?;
        NtfsVolumeAPI::read_volume_bytes(&self.raw, offset, output)
    }
    fn write_volume_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        offset
            .checked_add(data.len() as u64)
            .filter(|end| *end <= NtfsVolumeAPI::size_bytes(self))
            .ok_or_else(|| Error::invalid("write exceeds declared NTFS volume"))?;
        NtfsVolumeAPI::write_volume_bytes(&self.raw, offset, data)
    }
    fn is_writable(&self) -> bool {
        NtfsVolumeAPI::is_writable(&self.raw)
    }
    fn flush(&self) -> Result<(), Error> {
        NtfsVolumeAPI::flush(&self.raw)
    }
}

/// The name [`NtfsVolumeAPI`] had before rust-fs-core's shared `VolumeAPI`
/// (`fs_core::layers`), which [`crate::layers::NtfsVolume`] implements.
/// Kept for one release.
#[deprecated(
    since = "0.12.0",
    note = "use fs_ntfs::layers::NtfsVolume through fs_core::layers::VolumeAPI, or NtfsVolumeAPI for the NTFS-specific surface"
)]
pub trait VolumeAPI: NtfsVolumeAPI {
    fn size_bytes(&self) -> u64 {
        NtfsVolumeAPI::size_bytes(self)
    }
    fn geometry(&self) -> Result<BootParams, Error> {
        NtfsVolumeAPI::geometry(self)
    }
    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), Error> {
        NtfsVolumeAPI::read_volume_bytes(self, offset, output)
    }
    fn write_volume_bytes(&self, offset: u64, data: &[u8]) -> Result<(), Error> {
        NtfsVolumeAPI::write_volume_bytes(self, offset, data)
    }
    fn is_writable(&self) -> bool {
        NtfsVolumeAPI::is_writable(self)
    }
    fn flush(&self) -> Result<(), Error> {
        NtfsVolumeAPI::flush(self)
    }
}

#[allow(deprecated)]
impl<T: NtfsVolumeAPI + ?Sized> VolumeAPI for T {}
