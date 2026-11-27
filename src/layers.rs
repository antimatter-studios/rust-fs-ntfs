//! The three API layers of rust-fs-core (`fs_core::layers`) for NTFS
//! volumes (#1385).
//!
//! [`NtfsVolume`] is the volume layer ([`VolumeAPI`], [`OpenVolume`]), the
//! primitive layer ([`PrimitiveAPI`], with [`NtfsAdapter`] as its
//! [`SalvageAdapter`]) and the file layer ([`FileAPI`]). It is built on this
//! crate's own readers: [`NtfsPrimitiveAPI`] below the namespace, whose
//! records, attribute lists and LZNT1 units are what the salvage verbs read,
//! and the native read path for the namespace of a healthy volume. What is
//! NTFS's own -- records, streams, `$MFTMirr`, the allocation rebuilt from
//! the records -- stays on [`NtfsPrimitiveAPI`], which `NtfsVolume` also
//! offers through [`NtfsVolume::volume`].
//!
//! Nothing here writes. The source is a [`BlockRead`].

use crate::attr_io::{attr_flags, AttrType};
use crate::compression;
use crate::error::Error as NtfsError;
use crate::mft_io::{self, BootParams};
use crate::primitives::{
    AttributeValue, FileNameEvidence, MftMap, NtfsPrimitiveAPI, RecordEvidence, ResolvedRecord,
    SalvageStream,
};
use crate::read;
use crate::volume::{RawVolume, Volume};
use fs_core::layers::{
    check_bounds, ApiError, Candidate, CandidateKind, CandidateState, Capabilities, ClusterLayout,
    Damage, DamageKind, DamageReport, DirEntry, Encoding, Extent, ExtentWalk, FileAPI, Geometry,
    HealthState, Integrity, Metadata, NodeKind, OpenVolume, Parent, ParentClaim, PlanMetadata,
    PrimitiveAPI, Probe, Recognition, SalvageAdapter, SalvageError, ScanReport, Severity, Sink,
    Structure, StructureRole, SuperblockCopy, Timestamp, Tree, TreeNode, VolumeAPI, VolumeHealth,
    VolumeSupport, WalkEnd, WalkExtent,
};
use fs_core::BlockRead;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::ControlFlow;

/// The root directory's record.
const ROOT_RECORD: u64 = 5;
/// Records below this are the metafiles, which a listing leaves out.
const FIRST_USER_RECORD: u64 = 16;
const VOLUME_RECORD: u64 = 3;
const LOGFILE_RECORD: u64 = 2;
const BITMAP_RECORD: u64 = 6;
const BOOT_BYTES: usize = 512;
const VOLUME_IS_DIRTY: u16 = 0x0001;
/// The codec of an LZNT1 compression unit.
pub const CODEC_LZNT1: &str = "lznt1";

/// Why a layer call failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum LayerError {
    /// A refusal the layers share.
    Api(ApiError),
    /// The source failed a read.
    Device(fs_core::Error),
    /// This crate refused.
    Ntfs(NtfsError),
}

impl fmt::Display for LayerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => error.fmt(formatter),
            Self::Device(error) => error.fmt(formatter),
            Self::Ntfs(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(error) => Some(error),
            Self::Device(error) => Some(error),
            Self::Ntfs(_) => None,
        }
    }
}

impl From<ApiError> for LayerError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<fs_core::Error> for LayerError {
    fn from(error: fs_core::Error) -> Self {
        Self::Device(error)
    }
}

impl From<NtfsError> for LayerError {
    fn from(error: NtfsError) -> Self {
        Self::Ntfs(error)
    }
}

/// A borrowed read view, so the crate's own readers run over the `&dyn
/// BlockRead` each salvage verb is given.
struct View<'a>(&'a dyn BlockRead);

impl BlockRead for View<'_> {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        self.0.read_at(offset, output)
    }
    fn size_bytes(&self) -> u64 {
        self.0.size_bytes()
    }
}

fn raw_view(source: &dyn BlockRead) -> RawVolume<View<'_>> {
    RawVolume::open(View(source))
}

/// The boot sector at `offset` of `source`, decoded where it validates.
fn boot_at(source: &dyn BlockRead, offset: u64) -> Option<BootParams> {
    let mut sector = [0; BOOT_BYTES];
    source.read_at(offset, &mut sector).ok()?;
    mft_io::parse_boot_sector(&sector).ok()
}

/// The boot sector at `offset`: `Err` when it cannot be read, `Ok(None)`
/// when it reads and does not validate.
fn boot_read(source: &dyn BlockRead, offset: u64) -> Result<Option<BootParams>, fs_core::Error> {
    let mut sector = [0; BOOT_BYTES];
    source.read_at(offset, &mut sector)?;
    Ok(mft_io::parse_boot_sector(&sector).ok())
}

/// Whether the bytes of `extent` can be read.
fn readable(source: &dyn BlockRead, extent: Extent) -> bool {
    let mut bytes = vec![0; extent.length.min(1 << 20) as usize];
    source.read_at(extent.offset, &mut bytes).is_ok()
}

/// Where the backup boot sector is: the last sector of the volume the
/// primary declares, or, without one, the device's last 512 bytes.
fn backup_boot_offset(source: &dyn BlockRead, primary: Option<&BootParams>) -> Option<u64> {
    match primary {
        Some(boot) => boot
            .total_sectors
            .checked_mul(u64::from(boot.bytes_per_sector)),
        None => source.size_bytes().checked_sub(BOOT_BYTES as u64),
    }
}

fn record_map<V: NtfsPrimitiveAPI + ?Sized>(
    volume: &V,
    boot: &BootParams,
) -> Result<MftMap, NtfsError> {
    volume
        .read_mft_map(
            boot.mft_lcn
                .checked_mul(boot.cluster_size)
                .ok_or_else(|| NtfsError::invalid("$MFT cluster overflows"))?,
            boot.file_record_size as usize,
            boot.bytes_per_sector,
            boot.cluster_size,
        )
        .or_else(|_| volume.read_mft_map_from_mirror(boot))
}

fn cluster_count(boot: &BootParams) -> u64 {
    boot.total_sectors
        .saturating_mul(u64::from(boot.bytes_per_sector))
        / boot.cluster_size
}

fn timestamp(nt: u64) -> Option<Timestamp> {
    (nt != 0).then(|| {
        let ticks = nt % 10_000_000;
        Timestamp {
            seconds: read::nt_to_unix(nt),
            nanoseconds: (ticks * 100) as u32,
        }
    })
}

/// The name a record is best known by: Win32 over POSIX over DOS.
fn best_name(names: &[FileNameEvidence]) -> Option<&FileNameEvidence> {
    names.iter().min_by_key(|name| match name.namespace {
        1 | 3 => 0,
        0 => 1,
        _ => 2,
    })
}

/// The device bounded to the volume: the sectors the boot sector declares,
/// and the one after them, which holds the backup boot sector.
pub struct Region<D> {
    device: D,
    size: u64,
}

impl<D: BlockRead> Region<D> {
    fn new(device: D, boot: Option<&BootParams>) -> Self {
        let size = boot
            .and_then(|boot| {
                boot.total_sectors
                    .checked_add(1)?
                    .checked_mul(u64::from(boot.bytes_per_sector))
            })
            .map_or(device.size_bytes(), |declared| {
                declared.min(device.size_bytes())
            });
        Self { device, size }
    }

    /// The device, unbounded.
    pub fn device(&self) -> &D {
        &self.device
    }

    pub fn into_device(self) -> D {
        self.device
    }
}

impl<D: BlockRead> BlockRead for Region<D> {
    fn read_at(&self, offset: u64, output: &mut [u8]) -> fs_core::Result<()> {
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.size)
        {
            return Err(fs_core::Error::OutOfBounds {
                offset,
                len: output.len() as u64,
                size: self.size,
            });
        }
        self.device.read_at(offset, output)
    }
    fn size_bytes(&self) -> u64 {
        self.size
    }
}

/// An NTFS volume through rust-fs-core's three layers.
pub struct NtfsVolume<D: BlockRead> {
    volume: Volume<Region<D>>,
    /// Opened whole, so the namespace is offered.
    files: bool,
    /// Opened from the backup boot sector.
    from_backup: bool,
}

impl<D: BlockRead> NtfsVolume<D> {
    /// The crate's own volume, for the NTFS-specific primitives
    /// ([`NtfsPrimitiveAPI`]: records, streams, `$MFTMirr`, the allocation
    /// rebuilt from the records) and the writable surface.
    pub fn volume(&self) -> &Volume<Region<D>> {
        &self.volume
    }

    pub fn into_inner(self) -> Volume<Region<D>> {
        self.volume
    }

    /// Whether the boot parameters came from the backup boot sector.
    pub fn opened_from_backup(&self) -> bool {
        self.from_backup
    }

    fn device(&self) -> &Region<D> {
        self.volume.raw().device()
    }

    fn boot(&self) -> &BootParams {
        self.volume.boot()
    }

    fn require_files(&self) -> Result<(), LayerError> {
        if self.files {
            Ok(())
        } else {
            Err(ApiError::Unsupported("a namespace on a volume opened for salvage").into())
        }
    }

    fn read_io<T>(
        &self,
        operation: impl FnOnce(&mut crate::volume::Transport<'_, Region<D>>) -> Result<T, NtfsError>,
    ) -> Result<T, LayerError> {
        self.volume
            .raw()
            .with_io(operation)
            .map_err(LayerError::from)
    }
}

impl<D: BlockRead> VolumeAPI for NtfsVolume<D> {
    type Error = LayerError;
    type Superblock = BootParams;

    fn format(&self) -> &'static str {
        "ntfs"
    }

    /// The declared sectors and the backup boot sector after them.
    fn size_bytes(&self) -> u64 {
        self.device().size_bytes()
    }

    fn read_volume_bytes(&self, offset: u64, output: &mut [u8]) -> Result<(), LayerError> {
        check_bounds(offset, output.len(), VolumeAPI::size_bytes(self))?;
        self.device().read_at(offset, output)?;
        Ok(())
    }

    fn geometry(&self) -> Result<Geometry, LayerError> {
        let boot = self.boot();
        let clusters = cluster_count(boot);
        let label = self
            .volume
            .raw()
            .with_io(|io| crate::mft_io::mft_record_offset_io(io, boot, VOLUME_RECORD))
            .ok()
            .and_then(|offset| {
                self.volume
                    .read_file_record_at(
                        offset,
                        boot.file_record_size as usize,
                        boot.bytes_per_sector,
                    )
                    .ok()
            })
            .and_then(|record| record.volume_name())
            .filter(|label| !label.is_empty());
        Ok(Geometry {
            sector_size: u32::from(boot.bytes_per_sector),
            allocation_unit: boot.cluster_size,
            allocation_units: clusters,
            declared_bytes: boot
                .total_sectors
                .saturating_mul(u64::from(boot.bytes_per_sector)),
            layout: Some(ClusterLayout {
                first: 0,
                count: clusters,
                size: boot.cluster_size,
                offset: 0,
            }),
            label,
            serial: Some(format!("{:016X}", boot.serial_number)),
        })
    }

    fn superblock(&self) -> Result<BootParams, LayerError> {
        Ok(*self.boot())
    }

    fn superblocks(&self) -> Result<Vec<SuperblockCopy<BootParams>>, LayerError> {
        let source: &dyn BlockRead = self.device();
        let primary = boot_at(source, 0);
        let mut copies = vec![SuperblockCopy {
            copy: 0,
            location: Extent::new(0, BOOT_BYTES as u64),
            superblock: primary,
            integrity: if primary.is_some() {
                Integrity::Verified
            } else {
                Integrity::Failed
            },
        }];
        if let Some(offset) = backup_boot_offset(source, primary.as_ref().or(Some(self.boot()))) {
            let backup = boot_at(source, offset);
            copies.push(SuperblockCopy {
                copy: 1,
                location: Extent::new(offset, BOOT_BYTES as u64),
                superblock: backup,
                integrity: if backup.is_some() {
                    Integrity::Verified
                } else {
                    Integrity::Failed
                },
            });
        }
        Ok(copies)
    }

    fn support(&self) -> Result<VolumeSupport, LayerError> {
        Ok(VolumeSupport {
            files: self.files,
            salvage: NtfsAdapter::new(*self.boot()).capabilities(),
            unsupported: Vec::new(),
        })
    }

    fn health(&self) -> Result<VolumeHealth, LayerError> {
        let mut findings = Vec::new();
        for copy in self.superblocks()? {
            if copy.integrity == Integrity::Failed {
                findings.push(Damage {
                    kind: DamageKind::Signature,
                    severity: if copy.copy == 0 {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    location: copy.location,
                    structure: if copy.copy == 0 {
                        "boot sector"
                    } else {
                        "backup boot sector"
                    }
                    .into(),
                    detail: "the boot sector does not validate".into(),
                });
            }
        }
        if let Ok(info) = self.read_io(|io| read::read_volume_info(io)) {
            if info.flags & VOLUME_IS_DIRTY != 0 {
                findings.push(Damage {
                    kind: DamageKind::Recorded,
                    severity: Severity::Notice,
                    location: Extent::new(0, 0),
                    structure: "$Volume".into(),
                    detail: "the volume's dirty flag is set".into(),
                });
            }
        }
        match crate::evidence::logfile_state(self.device()) {
            Ok(state) if state.needs_replay() => findings.push(Damage {
                kind: DamageKind::Recorded,
                severity: Severity::Notice,
                location: Extent::new(0, 0),
                structure: "$LogFile".into(),
                detail: "the log holds work a replay would redo".into(),
            }),
            Ok(_) => {}
            Err(error) => findings.push(Damage {
                kind: DamageKind::Unreadable,
                severity: Severity::Warning,
                location: Extent::new(0, 0),
                structure: "$LogFile".into(),
                detail: format!("the log's state could not be read: {error}"),
            }),
        }
        Ok(VolumeHealth::from_findings(findings))
    }
}

impl<D: BlockRead> OpenVolume for NtfsVolume<D> {
    type Source = D;

    fn probe(source: &dyn BlockRead) -> Result<Probe, fs_core::Error> {
        let mut sector = [0; BOOT_BYTES];
        let read = source.read_at(0, &mut sector);
        let primary = read
            .as_ref()
            .ok()
            .and_then(|_| mft_io::parse_boot_sector(&sector).ok());
        let (recognition, declared, detail) = match primary {
            Some(boot) => (
                Recognition::Yes,
                boot.total_sectors
                    .checked_mul(u64::from(boot.bytes_per_sector)),
                "the boot sector validates".to_owned(),
            ),
            None => match backup_boot_offset(source, None).and_then(|at| boot_at(source, at)) {
                Some(boot) => (
                    Recognition::Damaged,
                    boot.total_sectors
                        .checked_mul(u64::from(boot.bytes_per_sector)),
                    "only the backup boot sector validates".to_owned(),
                ),
                None if read.is_ok() && &sector[3..11] == mft_io::NTFS_OEM_ID => (
                    Recognition::Damaged,
                    None,
                    "an NTFS boot sector whose geometry does not validate".to_owned(),
                ),
                None => (Recognition::No, None, "no NTFS boot sector".to_owned()),
            },
        };
        Ok(Probe {
            format: "ntfs",
            recognition,
            declared_bytes: declared,
            detail,
        })
    }

    fn open(source: D) -> Result<Self, LayerError> {
        let boot = boot_at(&source, 0);
        Ok(Self {
            volume: Volume::open(Region::new(source, boot.as_ref()))?,
            files: true,
            from_backup: false,
        })
    }

    /// The primary boot sector's geometry, or the backup's when the primary
    /// does not validate; nothing else needs to.
    fn open_for_salvage(source: D) -> Result<Self, LayerError> {
        if let Some(boot) = boot_at(&source, 0) {
            return Ok(Self {
                volume: Volume::open_for_salvage(Region::new(source, Some(&boot)))?,
                files: false,
                from_backup: false,
            });
        }
        let backup = backup_boot_offset(&source, None)
            .and_then(|at| boot_at(&source, at))
            .ok_or(LayerError::Ntfs(NtfsError::invalid(
                "neither boot sector validates",
            )))?;
        Ok(Self {
            volume: Volume::with_boot(RawVolume::open(Region::new(source, Some(&backup))), backup),
            files: false,
            from_backup: true,
        })
    }
}

/// What may be supposed about a volume whose boot sectors are both gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NtfsHypothesis {
    pub bytes_per_sector: u16,
    pub cluster_size: u64,
    pub file_record_size: u64,
    pub mft_lcn: u64,
    pub mft_mirror_lcn: u64,
    pub total_sectors: u64,
}

impl NtfsHypothesis {
    fn boot(&self) -> Result<BootParams, SalvageError> {
        let valid = self.bytes_per_sector.is_power_of_two()
            && (512..=4096).contains(&self.bytes_per_sector)
            && self.cluster_size.is_power_of_two()
            && self.cluster_size >= u64::from(self.bytes_per_sector)
            && self.file_record_size.is_power_of_two()
            && (512..=4096).contains(&self.file_record_size);
        if !valid {
            return Err(SalvageError::NoGeometry);
        }
        Ok(BootParams {
            bytes_per_sector: self.bytes_per_sector,
            sectors_per_cluster: self.cluster_size / u64::from(self.bytes_per_sector),
            cluster_size: self.cluster_size,
            mft_lcn: self.mft_lcn,
            mft_mirror_lcn: self.mft_mirror_lcn,
            file_record_size: self.file_record_size,
            total_sectors: self.total_sectors,
            serial_number: 0,
            index_block_size: 4096,
            oem_id: *mft_io::NTFS_OEM_ID,
        })
    }

    fn from_boot(boot: &BootParams) -> Self {
        Self {
            bytes_per_sector: boot.bytes_per_sector,
            cluster_size: boot.cluster_size,
            file_record_size: boot.file_record_size,
            mft_lcn: boot.mft_lcn,
            mft_mirror_lcn: boot.mft_mirror_lcn,
            total_sectors: boot.total_sectors,
        }
    }
}

impl<D: BlockRead> PrimitiveAPI for NtfsVolume<D> {
    type Adapter = NtfsAdapter;
    type Hypothesis = NtfsHypothesis;

    fn source(&self) -> &dyn BlockRead {
        self.device()
    }

    fn adapter(&self) -> Result<NtfsAdapter, SalvageError> {
        Ok(NtfsAdapter::new(*self.boot()))
    }

    fn hypotheses(&self) -> Result<Vec<NtfsHypothesis>, SalvageError> {
        let source: &dyn BlockRead = self.device();
        let mut found = Vec::new();
        for boot in [Some(*self.boot()), boot_at(source, 0)]
            .into_iter()
            .chain([
                backup_boot_offset(source, Some(self.boot())).and_then(|at| boot_at(source, at))
            ])
            .flatten()
        {
            let hypothesis = NtfsHypothesis::from_boot(&boot);
            if !found.contains(&hypothesis) {
                found.push(hypothesis);
            }
        }
        Ok(found)
    }

    fn adapter_with(&self, hypothesis: &NtfsHypothesis) -> Result<NtfsAdapter, SalvageError> {
        Ok(NtfsAdapter::new(hypothesis.boot()?))
    }

    fn structures(&self) -> Result<Vec<Structure>, SalvageError> {
        let source: &dyn BlockRead = self.device();
        let boot = *self.boot();
        let mut structures = Vec::new();
        for copy in VolumeAPI::superblocks(self).map_err(|_| SalvageError::NoGeometry)? {
            structures.push(Structure {
                name: if copy.copy == 0 {
                    "boot sector"
                } else {
                    "backup boot sector"
                }
                .into(),
                role: StructureRole::Boot,
                location: copy.location,
                copy: copy.copy,
                integrity: copy.integrity,
            });
        }
        let raw = raw_view(source);
        let map = record_map(&raw, &boot);
        let record_bytes = boot.file_record_size;
        let mft_at = boot.mft_lcn.saturating_mul(boot.cluster_size);
        structures.push(Structure {
            name: "$MFT".into(),
            role: StructureRole::RecordTable,
            location: Extent::new(mft_at, record_bytes),
            copy: 0,
            integrity: match &map {
                Ok(map) if !map.is_hypothesis() && map.limitations.is_empty() => {
                    Integrity::Verified
                }
                _ => Integrity::Failed,
            },
        });
        let mirror_at = boot.mft_mirror_offset().unwrap_or(0);
        let mirror = raw.read_mirror_record(&boot, 0);
        structures.push(Structure {
            name: "$MFTMirr".into(),
            role: StructureRole::RecordTable,
            location: Extent::new(mirror_at, record_bytes * boot.mft_mirror_records()),
            copy: 1,
            integrity: match mirror {
                Ok(record) if record.fixed.is_some() => Integrity::Verified,
                _ => Integrity::Failed,
            },
        });
        if let Ok(map) = &map {
            for (number, name, role) in [
                (LOGFILE_RECORD, "$LogFile", StructureRole::Journal),
                (BITMAP_RECORD, "$Bitmap", StructureRole::AllocationMap),
                (ROOT_RECORD, "root directory", StructureRole::Directory),
            ] {
                let record = raw.read_mapped_record(map, number);
                let (location, integrity) = match &record {
                    Ok(record) if record.fixed.is_some() => {
                        let first_run = record.attributes.iter().find_map(|attribute| {
                            match (
                                &attribute.value,
                                attribute.type_code == AttrType::Data as u32,
                            ) {
                                (AttributeValue::NonResident { runs, .. }, true) => {
                                    runs.iter().find_map(|run| {
                                        Some(Extent::new(
                                            run.lcn? * boot.cluster_size,
                                            run.length * boot.cluster_size,
                                        ))
                                    })
                                }
                                _ => None,
                            }
                        });
                        (
                            first_run.unwrap_or(Extent::new(record.offset, record_bytes)),
                            Integrity::Verified,
                        )
                    }
                    _ => (
                        Extent::new(map.record_offset(number).unwrap_or(0), record_bytes),
                        Integrity::Failed,
                    ),
                };
                structures.push(Structure {
                    name: name.into(),
                    role,
                    location,
                    copy: 0,
                    integrity,
                });
            }
        }
        Ok(structures)
    }
}

/// What a candidate is: one file record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NtfsCandidate {
    /// The record's number in `$MFT`.
    pub record: u64,
    /// The record's sequence number.
    pub sequence: u16,
}

/// NTFS's salvage adapter: the boot parameters, and nothing read from a
/// source; every verb reads the source it is given.
#[derive(Clone, Debug)]
pub struct NtfsAdapter {
    boot: Option<BootParams>,
}

impl NtfsAdapter {
    pub fn new(boot: BootParams) -> Self {
        Self { boot: Some(boot) }
    }

    /// The adapter for a source, from its boot sector, or its backup boot
    /// sector when the primary does not validate, or with no geometry at
    /// all, when every verb but `report_damage` refuses with `NoGeometry`.
    pub fn from_source(source: &dyn BlockRead) -> Self {
        let boot = boot_at(source, 0)
            .or_else(|| backup_boot_offset(source, None).and_then(|at| boot_at(source, at)));
        Self { boot }
    }

    /// The boot parameters, when the adapter has any.
    pub fn boot(&self) -> Option<&BootParams> {
        self.boot.as_ref()
    }

    /// The name and times a materialization plan carries for a candidate:
    /// `plan.metadata = adapter.plan_metadata(source, &candidate)?`.
    ///
    /// The name is the candidate's. Times are the base record's
    /// `$STANDARD_INFORMATION` FILETIMEs (100 ns since 1601-01-01 UTC) in
    /// decimal. A time is absent when it is zero, and all three are absent
    /// when the record cannot be read, does not validate, or has another
    /// sequence number than the candidate's.
    pub fn plan_metadata(
        &self,
        source: &dyn BlockRead,
        candidate: &Candidate<NtfsCandidate>,
    ) -> Result<PlanMetadata, SalvageError> {
        let raw = raw_view(source);
        let map = self.map(&raw)?;
        let times = self
            .resolve(&raw, &map, candidate.detail.record)
            .filter(|(base, _)| base.sequence == candidate.detail.sequence)
            .and_then(|(base, _)| base.standard_information());
        let time = |nt: u64| (nt != 0).then(|| nt.to_string());
        Ok(PlanMetadata {
            name: candidate.name.clone(),
            created: times.as_ref().and_then(|t| time(t.created_nt)),
            modified: times.as_ref().and_then(|t| time(t.modified_nt)),
            accessed: times.as_ref().and_then(|t| time(t.accessed_nt)),
        })
    }

    fn geometry(&self) -> Result<&BootParams, SalvageError> {
        self.boot.as_ref().ok_or(SalvageError::NoGeometry)
    }

    fn map(&self, raw: &RawVolume<View<'_>>) -> Result<MftMap, SalvageError> {
        record_map(raw, self.geometry()?).map_err(|_| SalvageError::NoGeometry)
    }

    fn resolve(
        &self,
        raw: &RawVolume<View<'_>>,
        map: &MftMap,
        record: u64,
    ) -> Option<(RecordEvidence, ResolvedRecord)> {
        let base = raw.read_mapped_record(map, record).ok()?;
        base.fixed.as_ref()?;
        let resolved = raw.resolve_salvage_record(&base, map).ok()?;
        Some((base, resolved))
    }

    fn walk_stream(
        &self,
        base: &RecordEvidence,
        stream: &SalvageStream,
        cluster: u64,
    ) -> ExtentWalk {
        let size = stream.len();
        let mut walk = ExtentWalk {
            extents: Vec::new(),
            claimed: size,
            end: WalkEnd::Complete,
        };
        if !stream.is_complete() || stream.is_encrypted() {
            walk.end = WalkEnd::Broken;
            return walk;
        }
        if let Some(value) = &stream.resident {
            match resident_extents(base, &stream.name, value.len(), cluster) {
                Some(extents) => walk.extents = extents,
                None => walk.end = WalkEnd::Broken,
            }
            return walk;
        }
        let initialized = stream.initialized_size.min(size);
        if stream.is_compressed() {
            if initialized < size
                || stream.flags & attr_flags::COMPRESSION_MASK != attr_flags::COMPRESSED
                || !(1..=4).contains(&stream.compression_unit)
            {
                walk.end = WalkEnd::Broken;
                return walk;
            }
            let unit_clusters = 1u64 << stream.compression_unit;
            let unit_bytes = unit_clusters * cluster;
            let mut unit = 0u64;
            while unit * unit_bytes < size {
                let first_vcn = unit * unit_clusters;
                let file_at = unit * unit_bytes;
                let length = unit_bytes.min(size - file_at);
                let lcns: Vec<Option<u64>> = (first_vcn..first_vcn + unit_clusters)
                    .map(|vcn| lcn_of(&stream.runs, vcn))
                    .collect();
                let allocated: Vec<u64> = lcns.iter().flatten().copied().collect();
                if allocated.is_empty() {
                    // A hole: zeros.
                } else if allocated.len() as u64 == unit_clusters {
                    push_clusters(&mut walk, &lcns, file_at, length, cluster);
                } else {
                    let contiguous = allocated.windows(2).all(|pair| pair[1] == pair[0] + 1);
                    if !contiguous {
                        walk.end = WalkEnd::Broken;
                        return walk;
                    }
                    walk.extents.push(WalkExtent {
                        offset: file_at,
                        source: Extent::new(
                            allocated[0] * cluster,
                            allocated.len() as u64 * cluster,
                        ),
                        uninitialized: false,
                        encoded: Some(Encoding {
                            codec: CODEC_LZNT1.into(),
                            decoded: length,
                            offset: 0,
                            length,
                        }),
                    });
                }
                unit += 1;
            }
            return walk;
        }
        for run in &stream.runs {
            let Some(lcn) = run.lcn else { continue };
            let start = run.starting_vcn * cluster;
            if start >= size {
                break;
            }
            let end = ((run.starting_vcn + run.length) * cluster).min(size);
            // Split at the initialized size: bytes past it read as zeros.
            for (from, to, uninitialized) in [
                (start, end.min(initialized.max(start)), false),
                (start.max(initialized), end, true),
            ] {
                if from < to {
                    walk.extents.push(WalkExtent {
                        offset: from,
                        source: Extent::new(lcn * cluster + (from - start), to - from),
                        uninitialized,
                        encoded: None,
                    });
                }
            }
        }
        walk
    }
}

/// The volume offset of `vcn` in a run list, or `None` for a hole.
fn lcn_of(runs: &[crate::data_runs::DataRun], vcn: u64) -> Option<u64> {
    runs.iter()
        .find(|run| vcn >= run.starting_vcn && vcn - run.starting_vcn < run.length)
        .and_then(|run| run.lcn.map(|lcn| lcn + vcn - run.starting_vcn))
}

/// A stored unit's clusters as plain extents, merging adjacent ones.
fn push_clusters(
    walk: &mut ExtentWalk,
    lcns: &[Option<u64>],
    file_at: u64,
    length: u64,
    cluster: u64,
) {
    let mut done = 0u64;
    for lcn in lcns.iter().flatten() {
        if done >= length {
            break;
        }
        let take = cluster.min(length - done);
        let source = lcn * cluster;
        match walk.extents.last_mut() {
            Some(last)
                if last.encoded.is_none()
                    && last.offset + last.source.length == file_at + done
                    && last.source.offset + last.source.length == source =>
            {
                last.source.length += take;
            }
            _ => walk.extents.push(WalkExtent {
                offset: file_at + done,
                source: Extent::new(source, take),
                uninitialized: false,
                encoded: None,
            }),
        }
        done += take;
    }
}

/// Where a resident `$DATA` value called `name` starts in its record.
fn resident_value_offset(record: &RecordEvidence, name: &str) -> Option<usize> {
    let fixed = record.fixed.as_ref()?;
    let attribute = record.attributes.iter().find(|attribute| {
        attribute.type_code == AttrType::Data as u32
            && attribute.name == name
            && matches!(attribute.value, AttributeValue::Resident(_))
    })?;
    let value = u16::from_le_bytes([
        fixed[attribute.offset + 0x14],
        fixed[attribute.offset + 0x15],
    ]);
    Some(attribute.offset + usize::from(value))
}

/// A resident value as source extents in the record as stored: its bytes
/// where they lie, except the last two of each update sequence stride,
/// which the stored record keeps in its update sequence array. `None` when
/// the record may not lie in one piece (a record larger than a cluster) or
/// its array does not describe it.
fn resident_extents(
    record: &RecordEvidence,
    name: &str,
    length: usize,
    cluster: u64,
) -> Option<Vec<WalkExtent>> {
    if record.raw.len() as u64 > cluster {
        return None;
    }
    let start = resident_value_offset(record, name)?;
    let raw = &record.raw;
    let usa_offset = usize::from(u16::from_le_bytes([raw[4], raw[5]]));
    let strides = usize::from(u16::from_le_bytes([raw[6], raw[7]])).checked_sub(1)?;
    if strides == 0 || !raw.len().is_multiple_of(strides) {
        return None;
    }
    let stride = raw.len() / strides;
    let mut extents: Vec<WalkExtent> = Vec::new();
    for at in start..start.checked_add(length)? {
        let within = at % stride;
        let stored = if within >= stride - 2 {
            usa_offset + 2 + 2 * (at / stride) + (within - (stride - 2))
        } else {
            at
        };
        let source = record.offset + stored as u64;
        let file = (at - start) as u64;
        match extents.last_mut() {
            Some(last) if last.source.offset + last.source.length == source => {
                last.source.length += 1
            }
            _ => extents.push(WalkExtent {
                offset: file,
                source: Extent::new(source, 1),
                uninitialized: false,
                encoded: None,
            }),
        }
    }
    Some(extents)
}

impl SalvageAdapter for NtfsAdapter {
    type Detail = NtfsCandidate;

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn find_candidates(
        &self,
        source: &dyn BlockRead,
        range: Extent,
        sink: &mut dyn Sink<Candidate<NtfsCandidate>>,
    ) -> Result<ScanReport, SalvageError> {
        if range
            .offset
            .checked_add(range.length)
            .is_none_or(|end| end > source.size_bytes())
        {
            return Err(SalvageError::Bounds);
        }
        if range.length == 0 {
            return Ok(ScanReport::default());
        }
        let range_end = range.offset.saturating_add(range.length);
        let raw = raw_view(source);
        let mut report = ScanReport::default();
        if self.boot.is_none() {
            // No geometry: when the boot sectors cannot be read, that is
            // the evidence.
            let lost: Vec<Extent> = [Some(0), backup_boot_offset(source, None)]
                .into_iter()
                .flatten()
                .map(|at| Extent::new(at, BOOT_BYTES as u64))
                .filter(|extent| {
                    extent.offset < range_end && extent.offset + extent.length > range.offset
                })
                .filter(|extent| !readable(source, *extent))
                .collect();
            if lost.is_empty() {
                return Err(SalvageError::NoGeometry);
            }
            report.unreadable = lost;
            return Ok(report);
        }
        let map = match self.map(&raw) {
            Ok(map) => map,
            Err(error) => {
                // The records cannot be found: what could not be read is
                // the evidence, when that is why.
                let boot = self.geometry()?;
                let lost: Vec<Extent> = [
                    boot.mft_lcn.checked_mul(boot.cluster_size),
                    boot.mft_mirror_offset(),
                ]
                .into_iter()
                .flatten()
                .map(|at| Extent::new(at, boot.file_record_size))
                .filter(|extent| {
                    extent.offset < range_end && extent.offset + extent.length > range.offset
                })
                .filter(|extent| !readable(source, *extent))
                .collect();
                if lost.is_empty() {
                    return Err(error);
                }
                report.unreadable = lost;
                return Ok(report);
            }
        };
        let records = map.records().unwrap_or(0);
        let size = self.geometry()?.file_record_size;
        for number in 0..records {
            let Some(offset) = map.record_offset(number) else {
                continue;
            };
            if offset < range.offset || offset.saturating_add(size) > range_end {
                continue;
            }
            let location = Extent::new(offset, size);
            let record = match raw.read_mapped_record(&map, number) {
                Ok(record) => record,
                Err(_) => {
                    report.unreadable.push(location);
                    continue;
                }
            };
            if record.raw.iter().all(|byte| *byte == 0) {
                continue;
            }
            let candidate = if record.fixed.is_none() {
                if &record.raw[..4] != b"FILE" && &record.raw[..4] != b"BAAD" {
                    continue;
                }
                Candidate {
                    location,
                    kind: CandidateKind::Other,
                    state: CandidateState::Unknown,
                    integrity: Integrity::Failed,
                    name: None,
                    size: None,
                    detail: NtfsCandidate {
                        record: number,
                        sequence: u16::from_le_bytes([record.raw[0x10], record.raw[0x11]]),
                    },
                }
            } else {
                if record.base_reference != 0 {
                    continue;
                }
                let resolved = raw.resolve_salvage_record(&record, &map).ok();
                let names = resolved
                    .as_ref()
                    .map(ResolvedRecord::file_names)
                    .unwrap_or_else(|| record.file_names());
                let size = (!record.is_directory)
                    .then(|| {
                        resolved
                            .as_ref()
                            .and_then(|r| r.stream(""))
                            .map(|s| s.len())
                    })
                    .flatten();
                Candidate {
                    location,
                    kind: if number < FIRST_USER_RECORD && number != ROOT_RECORD {
                        CandidateKind::Structure
                    } else if record.is_directory {
                        CandidateKind::Directory
                    } else {
                        CandidateKind::File
                    },
                    state: if record.in_use {
                        CandidateState::Live
                    } else {
                        CandidateState::Deleted
                    },
                    integrity: if record.issues.is_empty() {
                        Integrity::Verified
                    } else {
                        Integrity::Failed
                    },
                    name: best_name(&names).map(|name| name.name.clone()),
                    size,
                    detail: NtfsCandidate {
                        record: number,
                        sequence: record.sequence,
                    },
                }
            };
            report.candidates += 1;
            if let ControlFlow::Break(()) = sink.accept(candidate) {
                report.stopped_at = Some(offset.saturating_add(size));
                break;
            }
        }
        Ok(report)
    }

    fn walk_extents(
        &self,
        source: &dyn BlockRead,
        candidate: &Candidate<NtfsCandidate>,
    ) -> Result<ExtentWalk, SalvageError> {
        let raw = raw_view(source);
        let map = self.map(&raw)?;
        let Some((base, resolved)) = self.resolve(&raw, &map, candidate.detail.record) else {
            return Ok(ExtentWalk {
                extents: Vec::new(),
                claimed: candidate.size.unwrap_or(0),
                end: WalkEnd::Unreadable(candidate.location),
            });
        };
        if base.sequence != candidate.detail.sequence {
            return Ok(ExtentWalk {
                extents: Vec::new(),
                claimed: candidate.size.unwrap_or(0),
                end: WalkEnd::Broken,
            });
        }
        Ok(match resolved.stream("") {
            Some(stream) if !base.is_directory => {
                self.walk_stream(&base, &stream, self.geometry()?.cluster_size)
            }
            _ => ExtentWalk {
                extents: Vec::new(),
                claimed: 0,
                end: WalkEnd::Complete,
            },
        })
    }

    fn rebuild_tree(
        &self,
        source: &dyn BlockRead,
        candidates: &[Candidate<NtfsCandidate>],
    ) -> Result<Tree, SalvageError> {
        let raw = raw_view(source);
        let map = self.map(&raw)?;
        // Parents and nodes are named by the candidate's location offset.
        let index: std::collections::HashMap<u64, u64> = candidates
            .iter()
            .rev()
            .map(|candidate| (candidate.detail.record, candidate.location.offset))
            .collect();
        let mut nodes = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let mut parents = Vec::new();
            if candidate.detail.record == ROOT_RECORD {
                parents.push(ParentClaim {
                    parent: Parent::Root,
                    name: None,
                    state: candidate.state,
                });
            } else if let Some((_, resolved)) = self.resolve(&raw, &map, candidate.detail.record) {
                for name in resolved.file_names() {
                    if name.is_dos_only() {
                        continue;
                    }
                    let parent = if name.parent_record == ROOT_RECORD {
                        match index.get(&ROOT_RECORD) {
                            Some(root) => Parent::Candidate(*root),
                            None => Parent::Root,
                        }
                    } else {
                        match index.get(&name.parent_record) {
                            Some(parent) => Parent::Candidate(*parent),
                            None => continue,
                        }
                    };
                    parents.push(ParentClaim {
                        parent,
                        name: Some(name.name.clone().into_bytes()),
                        state: candidate.state,
                    });
                }
            }
            nodes.push(TreeNode {
                candidate: candidate.location.offset,
                parents,
            });
        }
        Ok(Tree { nodes })
    }

    fn report_damage(
        &self,
        source: &dyn BlockRead,
        sink: &mut dyn Sink<Damage>,
    ) -> Result<DamageReport, SalvageError> {
        let mut report = DamageReport::default();
        let mut emit = |damage: Damage, report: &mut DamageReport| -> bool {
            report.reported += 1;
            let stop = sink.accept(damage).is_break();
            report.stopped = stop;
            stop
        };
        let backup_at = backup_boot_offset(source, self.boot.as_ref());
        for (copy, at) in [(0u32, Some(0)), (1, backup_at)] {
            let Some(at) = at else { continue };
            let structure = if copy == 0 {
                "boot sector"
            } else {
                "backup boot sector"
            };
            let damage = match boot_read(source, at) {
                Ok(Some(_)) => continue,
                Ok(None) => Damage {
                    kind: DamageKind::Signature,
                    severity: if copy == 0 {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    location: Extent::new(at, BOOT_BYTES as u64),
                    structure: structure.into(),
                    detail: format!("the {structure} does not validate"),
                },
                Err(error) => Damage {
                    kind: DamageKind::Unreadable,
                    severity: if copy == 0 {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    location: Extent::new(at, BOOT_BYTES as u64),
                    structure: structure.into(),
                    detail: format!("the {structure} cannot be read: {error}"),
                },
            };
            if emit(damage, &mut report) {
                return Ok(report);
            }
        }
        let Some(boot) = self.boot.as_ref() else {
            return Ok(report);
        };
        let raw = raw_view(source);
        let map = match record_map(&raw, boot) {
            Ok(map) => map,
            Err(error) => {
                let at = Extent::new(
                    boot.mft_lcn.saturating_mul(boot.cluster_size),
                    boot.file_record_size,
                );
                emit(
                    Damage {
                        kind: if readable(source, at) {
                            DamageKind::Broken
                        } else {
                            DamageKind::Unreadable
                        },
                        severity: Severity::Error,
                        location: Extent::new(
                            boot.mft_lcn.saturating_mul(boot.cluster_size),
                            boot.file_record_size,
                        ),
                        structure: "$MFT".into(),
                        detail: format!("neither $MFT nor $MFTMirr maps the records: {error}"),
                    },
                    &mut report,
                );
                return Ok(report);
            }
        };
        for limitation in &map.limitations {
            let damage = Damage {
                kind: DamageKind::Broken,
                severity: Severity::Warning,
                location: Extent::new(
                    boot.mft_lcn.saturating_mul(boot.cluster_size),
                    boot.file_record_size,
                ),
                structure: "$MFT".into(),
                detail: limitation.clone(),
            };
            if emit(damage, &mut report) {
                return Ok(report);
            }
        }
        let size = boot.file_record_size;
        for number in 0..map.records().unwrap_or(0) {
            let Some(offset) = map.record_offset(number) else {
                continue;
            };
            let damage = match raw.read_mapped_record(&map, number) {
                Err(error) => Some(Damage {
                    kind: DamageKind::Unreadable,
                    severity: Severity::Error,
                    location: Extent::new(offset, size),
                    structure: format!("$MFT record {number}"),
                    detail: error.to_string(),
                }),
                Ok(record) if record.raw.iter().all(|byte| *byte == 0) => None,
                Ok(record) if record.fixed.is_none() => Some(Damage {
                    kind: if &record.raw[..4] == b"FILE" {
                        DamageKind::Checksum
                    } else {
                        DamageKind::Signature
                    },
                    severity: Severity::Error,
                    location: Extent::new(offset, size),
                    structure: format!("$MFT record {number}"),
                    detail: record.issues.join("; "),
                }),
                Ok(record) if !record.issues.is_empty() && record.in_use => Some(Damage {
                    kind: DamageKind::Malformed,
                    severity: Severity::Error,
                    location: Extent::new(offset, size),
                    structure: format!("$MFT record {number}"),
                    detail: record.issues.join("; "),
                }),
                Ok(_) => None,
            };
            if let Some(damage) = damage {
                if emit(damage, &mut report) {
                    return Ok(report);
                }
            }
        }
        Ok(report)
    }

    fn read_extent(
        &self,
        source: &dyn BlockRead,
        extent: Extent,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, SalvageError> {
        if offset > extent.length {
            return Err(SalvageError::Bounds);
        }
        let count = (extent.length - offset).min(output.len() as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let at = extent
            .offset
            .checked_add(offset)
            .ok_or(SalvageError::Bounds)?;
        if at
            .checked_add(count as u64)
            .is_none_or(|end| end > source.size_bytes())
        {
            return Err(SalvageError::Bounds);
        }
        source
            .read_at(at, &mut output[..count])
            .map_err(SalvageError::Device)?;
        Ok(count)
    }

    fn read_encoded(
        &self,
        source: &dyn BlockRead,
        stored: Extent,
        encoding: &Encoding,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, SalvageError> {
        if offset > encoding.length {
            return Err(SalvageError::Bounds);
        }
        if stored
            .offset
            .checked_add(stored.length)
            .is_none_or(|end| end > source.size_bytes())
            || stored.length > 1 << 24
        {
            return Err(SalvageError::Bounds);
        }
        let mut bytes = vec![0; stored.length as usize];
        source
            .read_at(stored.offset, &mut bytes)
            .map_err(SalvageError::Device)?;
        let decoded = match encoding.codec.as_str() {
            CODEC_LZNT1 => compression::decompress_unit(&bytes, encoding.decoded as usize)
                .map_err(|error| {
                    SalvageError::Device(fs_core::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "LZNT1 unit at byte {} does not decode: {error}",
                            stored.offset
                        ),
                    )))
                })?,
            _ => {
                return Err(SalvageError::Unsupported(
                    fs_core::layers::Feature::EncodedExtents,
                ))
            }
        };
        let mut decoded = decoded;
        // A unit that decodes short holds zeros to its decoded length.
        if (decoded.len() as u64) < encoding.decoded {
            decoded.resize(encoding.decoded as usize, 0);
        }
        let start = encoding.offset + offset;
        let end = (encoding.offset + encoding.length).min(decoded.len() as u64);
        if start > end {
            return Err(SalvageError::Bounds);
        }
        let count = ((end - start) as usize).min(output.len());
        output[..count].copy_from_slice(&decoded[start as usize..start as usize + count]);
        Ok(count)
    }

    fn cluster_layout(&self) -> Result<ClusterLayout, SalvageError> {
        let boot = self.geometry()?;
        Ok(ClusterLayout {
            first: 0,
            count: cluster_count(boot),
            size: boot.cluster_size,
            offset: 0,
        })
    }
}

/// A file or directory, by its record and sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NtfsNode {
    pub record: u64,
    pub sequence: u16,
}

impl<D: BlockRead> NtfsVolume<D> {
    fn stat(&self, node: &NtfsNode) -> Result<read::Stat, LayerError> {
        self.read_io(|io| read::read_stat(io, node.record))
    }
}

impl<D: BlockRead> FileAPI for NtfsVolume<D> {
    type Node = NtfsNode;

    fn root(&self) -> Result<NtfsNode, LayerError> {
        self.require_files()?;
        let sequence = self.read_io(|io| {
            let (_, record) = mft_io::read_mft_record_io(io, ROOT_RECORD)?;
            Ok(mft_io::record_sequence(&record))
        })?;
        Ok(NtfsNode {
            record: ROOT_RECORD,
            sequence,
        })
    }

    fn read_dir(&self, directory: &NtfsNode) -> Result<Vec<DirEntry<NtfsNode>>, LayerError> {
        self.require_files()?;
        let stat = self.stat(directory)?;
        if !stat.is_dir {
            return Err(ApiError::NotADirectory(format!("record {}", directory.record)).into());
        }
        let entries = self.read_io(|io| read::read_dir_entries(io, directory.record))?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.name == "." || entry.name == ".." || entry.record_number < FIRST_USER_RECORD {
                continue;
            }
            let (sequence, stat) = self.read_io(|io| {
                let (_, record) = mft_io::read_mft_record_io(io, entry.record_number)?;
                Ok((
                    mft_io::record_sequence(&record),
                    read::read_stat(io, entry.record_number)?,
                ))
            })?;
            out.push(DirEntry {
                bytes: entry.name.clone().into_bytes(),
                name: entry.name,
                lossy: false,
                kind: if stat.is_dir {
                    NodeKind::Directory
                } else {
                    NodeKind::File
                },
                size: if stat.is_dir { 0 } else { stat.size },
                node: NtfsNode {
                    record: entry.record_number,
                    sequence,
                },
            });
        }
        Ok(out)
    }

    fn metadata(&self, node: &NtfsNode) -> Result<Metadata, LayerError> {
        self.require_files()?;
        let stat = self.stat(node)?;
        let mut metadata = Metadata::new(
            if stat.is_dir {
                NodeKind::Directory
            } else {
                NodeKind::File
            },
            if stat.is_dir { 0 } else { stat.size },
        );
        metadata.created = timestamp(stat.created_nt);
        metadata.modified = timestamp(stat.modified_nt);
        metadata.accessed = timestamp(stat.accessed_nt);
        metadata.links = Some(u64::from(stat.link_count));
        metadata.attributes = Some(u64::from(stat.file_attributes));
        Ok(metadata)
    }

    fn read_at(
        &self,
        file: &NtfsNode,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, LayerError> {
        self.require_files()?;
        let stat = self.stat(file)?;
        if stat.is_dir {
            return Err(ApiError::IsADirectory(format!("record {}", file.record)).into());
        }
        if offset >= stat.size || output.is_empty() {
            return Ok(0);
        }
        let length = (stat.size - offset).min(output.len() as u64) as usize;
        let bytes = self.read_io(|io| {
            read::read_attribute_range(io, file.record, AttrType::Data, None, offset, length)
        })?;
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    /// NTFS compares names case-insensitively.
    fn names_match(&self, stored: &str, wanted: &str) -> bool {
        stored == wanted || stored.to_uppercase() == wanted.to_uppercase()
    }
}

/// The health a fresh volume reports, for callers that only want the state.
pub fn health_state<D: BlockRead>(volume: &NtfsVolume<D>) -> HealthState {
    volume
        .health()
        .map_or(HealthState::Unknown, |health| health.state)
}
