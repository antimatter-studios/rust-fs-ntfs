// The structure-aware explorer (#483), shared by both tiers the way
// walk.rs is: the `structured` cargo-fuzz target and tests/fuzz_decoders.rs
// each `include!` this file inside a module of its own.
//
// The byte-wise `image` target barely reaches a parser. Every MFT record
// and index block carries an update-sequence array, so a random byte
// inside one breaks the sector-end check, or lands outside any field that
// means anything, and the case is thrown out at the fixup before an
// attribute, a run list or an index entry is read. And nothing writes:
// the `image` target's device refuses every write.
//
// This explorer differs in three ways:
//
// - It finds the structures on the volume itself, without the driver --
//   every 512-byte boundary that starts `FILE`, `INDX`, `RCRD` or `RSTR`,
//   sized by its own update-sequence count -- and mutates them with their
//   fixups undone, then stamps the fixups back, so the mutation survives
//   the check and reaches the parser. Three cases in a hundred are left
//   torn, so the fixup path itself stays covered.
// - It aims at fields: a record's header, an attribute's header (resident
//   and non-resident), an index header and its entries, a run list, the
//   boot sector's geometry, one structure spliced over another, and
//   single bit flips. Values come from a table of the ones that break
//   arithmetic, from the current value plus or minus a little, or from a
//   bit flip.
// - It reads everything a caller would, then writes through a device that
//   accepts writes -- create, mkdir, write, truncate, unlink, rename, a
//   named stream, an extended attribute, fsck and log replay -- and reads
//   everything again.
//
// The fuzz input is the recipe, not the volume: which seed volume, which
// mutations, which writes. A recipe is small, so libFuzzer can explore
// them quickly, and the volumes stay real.

use fs_ntfs::block_io::BlockIo;

/// A volume held in memory that takes writes inside its length.
pub struct Disk(pub Vec<u8>);

impl BlockIo for Disk {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(buf.len());
        if end > self.0.len() {
            return Err(format!(
                "read of {} bytes at {offset} runs past the end of a {}-byte volume",
                buf.len(),
                self.0.len()
            ));
        }
        buf.copy_from_slice(&self.0[start..end]);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start.saturating_add(buf.len());
        if end > self.0.len() {
            return Err(format!(
                "write of {} bytes at {offset} runs past the end of a {}-byte volume",
                buf.len(),
                self.0.len()
            ));
        }
        self.0[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.0.len() as u64
    }
}

/// The fuzz input, read a field at a time. Past its end every read is
/// zero, so a short recipe is a valid one.
pub struct Recipe<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Recipe<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Recipe { bytes, at: 0 }
    }

    fn u8(&mut self) -> u8 {
        let b = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        b
    }

    fn u64(&mut self) -> u64 {
        (0..8).fold(0u64, |v, i| v | u64::from(self.u8()) << (8 * i))
    }

    /// A number below `bound`, or 0 when `bound` is 0.
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let raw = u32::from(self.u8()) | u32::from(self.u8()) << 8;
        raw as usize % bound
    }
}

/// The sector a fixup protects.
const SECTOR: usize = 512;

/// A multi-sector structure on the volume, found by its signature.
#[derive(Debug, Clone, Copy)]
pub struct Found {
    pub at: usize,
    pub len: usize,
    pub magic: [u8; 4],
}

/// Every `FILE`, `INDX`, `RCRD` and `RSTR` structure on the volume, at
/// 512-byte boundaries, each sized by its own update-sequence count.
pub fn discover(image: &[u8]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + SECTOR <= image.len() {
        let magic: [u8; 4] = image[at..at + 4].try_into().expect("four bytes");
        if [*b"FILE", *b"INDX", *b"RCRD", *b"RSTR"].contains(&magic) {
            let usa_ofs = usize::from(u16::from_le_bytes([image[at + 4], image[at + 5]]));
            let usa_count = usize::from(u16::from_le_bytes([image[at + 6], image[at + 7]]));
            // One word for the sequence number, one per sector; at most a
            // 64 KiB structure.
            if (2..=129).contains(&usa_count) {
                let len = (usa_count - 1) * SECTOR;
                if at + len <= image.len() && usa_ofs + 2 * usa_count <= len {
                    out.push(Found { at, len, magic });
                    at += len;
                    continue;
                }
            }
        }
        at += SECTOR;
    }
    out
}

fn usa(image: &[u8], s: &Found) -> (usize, usize) {
    let usa_ofs = usize::from(u16::from_le_bytes([image[s.at + 4], image[s.at + 5]]));
    (s.at + usa_ofs, s.len / SECTOR)
}

/// Put back the bytes the fixup moved into the update-sequence array, so
/// the structure reads as it was before it was written.
fn unfix(image: &mut [u8], s: &Found) {
    let (array, sectors) = usa(image, s);
    for i in 1..=sectors {
        let end = s.at + i * SECTOR - 2;
        image.copy_within(array + 2 * i..array + 2 * i + 2, end);
    }
}

/// Write the fixup again: each sector's last word into the array, and the
/// sequence number over it.
fn refix(image: &mut [u8], s: &Found) {
    let (array, sectors) = usa(image, s);
    let usn = [image[array], image[array + 1]];
    for i in 1..=sectors {
        let end = s.at + i * SECTOR - 2;
        image.copy_within(end..end + 2, array + 2 * i);
        image[end..end + 2].copy_from_slice(&usn);
    }
}

/// The values that break arithmetic: zero, one, the powers of two a size
/// or an offset is made of, the signed and unsigned edges, and the record
/// and cluster sizes a volume is measured in.
const INTERESTING: &[u64] = &[
    0,
    1,
    2,
    7,
    8,
    0x10,
    0x18,
    0x7f,
    0x80,
    0xff,
    0x100,
    0x200,
    0x3ff,
    0x400,
    0x1000,
    0x7fff,
    0x8000,
    0xffff,
    0x1_0000,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_ffff,
    0x1_0000_0000,
    0x7fff_ffff_ffff_ffff,
    0x8000_0000_0000_0000,
    u64::MAX,
];

/// Overwrite the `width`-byte little-endian field at `at` with a value
/// chosen by the recipe: from [`INTERESTING`], the current value plus or
/// minus up to eight, or the current value with one bit flipped.
fn field(image: &mut [u8], at: usize, width: usize, r: &mut Recipe) {
    if at.checked_add(width).is_none_or(|end| end > image.len()) {
        return;
    }
    let mut buf = [0u8; 8];
    buf[..width].copy_from_slice(&image[at..at + width]);
    let current = u64::from_le_bytes(buf);
    let value = match r.below(3) {
        0 => INTERESTING[r.below(INTERESTING.len())],
        1 => {
            let delta = 1 + r.below(8) as u64;
            if r.below(2) == 0 {
                current.wrapping_add(delta)
            } else {
                current.wrapping_sub(delta)
            }
        }
        _ => current ^ (1u64 << r.below(width * 8)),
    };
    image[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
}

fn le16(image: &[u8], at: usize) -> usize {
    image
        .get(at..at + 2)
        .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])))
}

fn le32(image: &[u8], at: usize) -> usize {
    image
        .get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// The start of the `n`th attribute of the record at `s`, walking the
/// lengths the record declares; `None` when the walk stops first.
fn attribute(image: &[u8], s: &Found, n: usize) -> Option<usize> {
    let mut at = s.at + le16(image, s.at + 0x14);
    for _ in 0..n {
        let len = le32(image, at + 4);
        if len < 0x18 || le32(image, at) == 0xffff_ffff {
            return None;
        }
        at = at.checked_add(len)?;
        if at + 0x18 > s.at + s.len {
            return None;
        }
    }
    (at + 0x18 <= s.at + s.len && le32(image, at) != 0xffff_ffff).then_some(at)
}

/// The start of the `n`th entry of the index block at `s`.
fn index_entry(image: &[u8], s: &Found, n: usize) -> Option<usize> {
    let header = s.at + 0x18;
    let mut at = header.checked_add(le32(image, header))?;
    for _ in 0..n {
        let len = le16(image, at + 8);
        if len < 0x10 || le16(image, at + 12) & 2 != 0 {
            return None;
        }
        at = at.checked_add(len)?;
        if at + 0x10 > s.at + s.len {
            return None;
        }
    }
    (at + 0x10 <= s.at + s.len).then_some(at)
}

/// One mutation, chosen by the recipe, of one structure on the volume.
fn mutate(image: &mut [u8], found: &[Found], r: &mut Recipe) {
    if found.is_empty() {
        return;
    }
    let kind = r.below(8);
    if kind == 7 {
        // The boot sector's geometry, which no fixup protects.
        const BOOT: &[(usize, usize)] = &[
            (0x0b, 2),
            (0x0d, 1),
            (0x28, 8),
            (0x30, 8),
            (0x38, 8),
            (0x40, 1),
            (0x44, 1),
        ];
        let (at, width) = BOOT[r.below(BOOT.len())];
        field(image, at, width, r);
        return;
    }

    let s = found[r.below(found.len())];
    unfix(image, &s);
    match kind {
        // The record header: sequence number, link count, first
        // attribute, flags, bytes used, bytes allocated, base record,
        // next attribute id.
        0 if &s.magic == b"FILE" => {
            const HEADER: &[(usize, usize)] = &[
                (0x10, 2),
                (0x12, 2),
                (0x14, 2),
                (0x16, 2),
                (0x18, 4),
                (0x1c, 4),
                (0x20, 8),
                (0x28, 2),
            ];
            let (at, width) = HEADER[r.below(HEADER.len())];
            field(image, s.at + at, width, r);
        }
        // An attribute's header, resident or not.
        1 if &s.magic == b"FILE" => {
            if let Some(attr) = attribute(image, &s, r.below(8)) {
                const COMMON: &[(usize, usize)] =
                    &[(0, 4), (4, 4), (8, 1), (9, 1), (10, 2), (12, 2), (14, 2)];
                const RESIDENT: &[(usize, usize)] = &[(16, 4), (20, 2)];
                const NONRESIDENT: &[(usize, usize)] = &[
                    (16, 8),
                    (24, 8),
                    (32, 2),
                    (34, 2),
                    (40, 8),
                    (48, 8),
                    (56, 8),
                ];
                let own = if image[attr + 8] == 0 {
                    RESIDENT
                } else {
                    NONRESIDENT
                };
                let all: Vec<(usize, usize)> = COMMON.iter().chain(own).copied().collect();
                let (at, width) = all[r.below(all.len())];
                field(image, attr + at, width, r);
            }
        }
        // A run list's bytes.
        2 if &s.magic == b"FILE" => {
            if let Some(attr) = attribute(image, &s, r.below(8)) {
                if image[attr + 8] != 0 {
                    let runs = attr + le16(image, attr + 32);
                    let end = attr + le32(image, attr + 4);
                    if runs < end && end <= s.at + s.len {
                        field(image, runs + r.below(end - runs), 1, r);
                    }
                }
            }
        }
        // An index block's header: entries offset, index length,
        // allocated size, flags.
        0..=2 if &s.magic == b"INDX" => {
            const HEADER: &[(usize, usize)] = &[(0x18, 4), (0x1c, 4), (0x20, 4), (0x24, 1)];
            let (at, width) = HEADER[r.below(HEADER.len())];
            field(image, s.at + at, width, r);
        }
        // An index entry: file reference, length, key length, flags, and
        // the subnode VCN in its last eight bytes.
        3 if &s.magic == b"INDX" => {
            if let Some(entry) = index_entry(image, &s, r.below(16)) {
                let len = le16(image, entry + 8);
                let fields = [
                    (0usize, 8usize),
                    (8, 2),
                    (10, 2),
                    (12, 2),
                    (len.saturating_sub(8), 8),
                ];
                let (at, width) = fields[r.below(fields.len())];
                field(image, entry + at, width, r);
            }
        }
        // One structure spliced over another of the same kind.
        4 => {
            let same: Vec<&Found> = found
                .iter()
                .filter(|o| o.magic == s.magic && o.len == s.len && o.at != s.at)
                .collect();
            if !same.is_empty() {
                let from = *same[r.below(same.len())];
                unfix(image, &from);
                image.copy_within(from.at..from.at + from.len, s.at);
                refix(image, &from);
            }
        }
        // One bit anywhere in it.
        _ => {
            let at = s.at + r.below(s.len);
            image[at] ^= 1 << r.below(8);
        }
    }
    // Stamp the fixup again, except three times in a hundred.
    if r.below(100) < 97 {
        refix(image, &s);
    }
}

/// How many directory entries and paths one pass reads, so a crafted
/// directory that lists a million names makes a case slow rather than a
/// hang.
const PATH_BUDGET: usize = 24;

/// Read what a caller would: the volume, the root directory and every
/// name in it, each file's attributes, a few windows of its contents, its
/// named streams, extended attributes, reparse point and object id, then
/// the checker and the log's state. Every result is discarded.
fn read_all(io: &mut Disk) -> Vec<String> {
    use fs_ntfs::attr_io::AttrType;
    use fs_ntfs::read;

    let _ = read::read_volume_info(io);
    let mut paths = vec!["/".to_string()];
    if let Ok(entries) = read::read_dir_entries(io, read::ROOT_RECORD_NUMBER) {
        for entry in entries.into_iter().take(PATH_BUDGET) {
            let _ = read::read_stat(io, entry.record_number);
            if entry.is_dir {
                let _ = read::read_dir_entries(io, entry.record_number);
            }
            for (offset, len) in [(0u64, 512usize), (4095, 2), (65_536, 4096)] {
                let _ = read::read_attribute_range(
                    io,
                    entry.record_number,
                    AttrType::Data,
                    None,
                    offset,
                    len,
                );
            }
            paths.push(format!("/{}", entry.name));
        }
    }
    for path in &paths {
        let _ = read::resolve_path(io, path);
        let _ = fs_ntfs::write::list_named_streams_io(io, path);
        let _ = fs_ntfs::write::list_eas_io(io, path);
        let _ = fs_ntfs::write::read_reparse_point_io(io, path);
        let _ = fs_ntfs::write::read_object_id_io(io, path);
    }
    let _ = fs_ntfs::fsck::check_io(io);
    let _ = fs_ntfs::fsck::logfile_state_io(io);
    paths
}

/// Names a write may use: short, long, and one that is not ASCII.
const NAMES: &[&str] = &[
    "a",
    "b",
    "dir",
    "a-much-longer-name-for-the-index.txt",
    "naïve",
];

/// One write, chosen by the recipe, on a path the volume has or one of
/// [`NAMES`].
fn write_one(io: &mut Disk, paths: &[String], r: &mut Recipe) {
    use fs_ntfs::write;

    let name = NAMES[r.below(NAMES.len())];
    let path = if !paths.is_empty() && r.below(2) == 0 {
        paths[r.below(paths.len())].clone()
    } else {
        format!("/{name}")
    };
    let data = vec![r.u8(); r.below(70_000)];
    let _ = match r.below(10) {
        0 => write::create_file_io(io, "/", name).map(drop),
        1 => write::mkdir_io(io, "/", name).map(drop),
        2 => write::write_file_contents_io(io, &path, &data).map(drop),
        3 => write::truncate_io(io, &path, r.u64() % (1 << 21)).map(drop),
        4 => write::unlink_io(io, &path),
        5 => write::rename_io(io, &path, name),
        6 => write::write_named_stream_io(io, &path, name, &data),
        7 => write::write_ea_io(io, &path, name.as_bytes(), &data[..data.len().min(512)], 0),
        8 => fs_ntfs::fsck::fsck_io(io, None).map(drop),
        _ => fs_ntfs::fsck::replay_logfile_io(io).map(drop),
    };
}

/// A volume this crate formatted and filled with one of each thing the
/// reads and writes reach: a directory, a file with contents, a named
/// stream and an extended attribute.
///
/// 16 MiB: mkfs keeps its metadata below `$MFTMirr` at the volume's
/// midpoint, and since `$Secure:$SDS` became one contiguous 65-cluster
/// run (#837) that region ends at LCN 1081 with 4 KiB clusters and
/// 1 KiB records -- past the 8 MiB midpoint of 1024.
pub fn own_volume() -> Vec<u8> {
    let size = 16 << 20;
    let mut io = Disk(vec![0; size]);
    fs_ntfs::mkfs::format_filesystem(&mut io, size as u64, 4096, 1024, Some("FUZZ"), Some(1))
        .expect("format the explorer's own volume");
    let _ = fs_ntfs::write::mkdir_io(&mut io, "/", "dir");
    let _ = fs_ntfs::write::create_file_io(&mut io, "/", "file.txt");
    let _ = fs_ntfs::write::write_file_contents_io(&mut io, "/file.txt", &[0x5a; 20_000]);
    let _ = fs_ntfs::write::create_file_io(&mut io, "/dir", "small.txt");
    let _ = fs_ntfs::write::write_file_contents_io(&mut io, "/dir/small.txt", b"resident");
    let _ = fs_ntfs::write::write_named_stream_io(&mut io, "/file.txt", "s", b"a stream");
    let _ = fs_ntfs::write::write_ea_io(&mut io, "/file.txt", b"user.x", b"an attribute", 0);
    io.0
}

/// The seed volumes: every `.img` under `dir`, and [`own_volume`].
pub fn volumes(dir: &std::path::Path) -> Vec<Vec<u8>> {
    let mut names: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    names.retain(|p| p.extension().is_some_and(|e| e == "img"));
    names.sort();
    let mut out: Vec<Vec<u8>> = names.iter().filter_map(|p| std::fs::read(p).ok()).collect();
    out.push(own_volume());
    out
}

/// One case: a seed volume, mutated as the recipe says, read, written to
/// and read again. A crafted volume is supposed to be refused; what it
/// may not do is panic, hang, or allocate without bound.
pub fn explore(seeds: &[Vec<u8>], recipe: &[u8]) {
    if seeds.is_empty() {
        return;
    }
    let mut r = Recipe::new(recipe);
    let mut image = seeds[r.below(seeds.len())].clone();
    let found = discover(&image);
    for _ in 0..1 + r.below(8) {
        mutate(&mut image, &found, &mut r);
    }
    let mut io = Disk(image);
    let paths = read_all(&mut io);
    for _ in 0..3 + r.below(8) {
        write_one(&mut io, &paths, &mut r);
    }
    read_all(&mut io);
}
