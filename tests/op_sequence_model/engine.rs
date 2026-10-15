//! The engine: a seeded generator of operations, the in-memory reference
//! model they are applied to, and the comparison of a volume with the
//! model through this crate's own read layer.
//!
//! The independent readers -- the upstream `ntfs` crate and ntfs-3g --
//! are in `oracle.rs`; `main.rs` decides which sequences run.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use fs_ntfs::attr_io::AttrType;
use fs_ntfs::block_io::BlockIo;
use fs_ntfs::error::{Error, Kind};
use fs_ntfs::upcase::UpcaseTable;
use fs_ntfs::{fsck, mkfs, read, write};

// ---------------------------------------------------------------------------
// The volume
// ---------------------------------------------------------------------------

/// A volume held in memory. `vec![0; n]` is a zeroed allocation the
/// kernel hands out lazily, so a 256 MiB volume costs only the pages the
/// format and the operations touch.
pub struct MemIo {
    pub bytes: Vec<u8>,
}

impl MemIo {
    fn range(&self, offset: u64, len: usize) -> Result<std::ops::Range<usize>, String> {
        usize::try_from(offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(len)?))
            .filter(|r| r.end <= self.bytes.len())
            .ok_or_else(|| {
                format!(
                    "{len} bytes at {offset} run past the {}-byte volume",
                    self.bytes.len()
                )
            })
    }
}

impl BlockIo for MemIo {
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let r = self.range(offset, buf.len())?;
        buf.copy_from_slice(&self.bytes[r]);
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), String> {
        let r = self.range(offset, buf.len())?;
        self.bytes[r].copy_from_slice(buf);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

/// Size and layout of a volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub size: u64,
    pub cluster: u32,
    pub record: u32,
}

impl Geometry {
    /// 64 to 256 MiB, every cluster size from 512 bytes to 64 KiB that
    /// `format_filesystem` gives at least its 1,024 clusters at 64 MiB,
    /// and both record sizes.
    pub fn from_rng(rng: &mut Rng) -> Self {
        const MIB: u64 = 1024 * 1024;
        Geometry {
            size: *rng.pick(&[64, 96, 128, 192, 256]) * MIB,
            cluster: *rng.pick(&[512, 1024, 2048, 4096, 4096, 65536]),
            record: *rng.pick(&[1024, 1024, 4096]),
        }
    }
}

impl fmt::Display for Geometry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} MiB, {}-byte clusters, {}-byte records",
            self.size >> 20,
            self.cluster,
            self.record
        )
    }
}

// ---------------------------------------------------------------------------
// Randomness
// ---------------------------------------------------------------------------

/// SplitMix64: small, seedable, and the same on every platform, which is
/// all a reproducer needs from it.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x6A09_E667_F3BC_C908)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`; `n` is never 0 here.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// A number in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// The bytes an operation writes, made from `(fill, len)` so a
/// reproducer can name them in a line of text. One fill in eight is all
/// zeros and one a single repeated byte; the rest are pseudo-random.
pub fn fill_bytes(fill: u64, len: usize) -> Vec<u8> {
    match fill % 8 {
        0 => vec![0; len],
        1 => vec![(fill >> 8) as u8 | 1; len],
        _ => {
            let mut rng = Rng::new(fill);
            let mut v = Vec::with_capacity(len + 8);
            while v.len() < len {
                v.extend_from_slice(&rng.next_u64().to_le_bytes());
            }
            v.truncate(len);
            v
        }
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// One call into this crate's write API. Paths are absolute; names are
/// single components. Every field is what the call is given, so a
/// sequence written out as text replays exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Create { parent: String, name: String },
    Mkdir { parent: String, name: String },
    /// `write_file_contents_io`.
    Write { path: String, len: usize, fill: u64 },
    /// `replace_file_contents_io`.
    Replace { path: String, len: usize, fill: u64 },
    WriteAt { path: String, offset: u64, len: usize, fill: u64 },
    Truncate { path: String, size: u64 },
    /// `rename_replace_io`: a new name in the same directory.
    Rename { path: String, name: String, replace: bool },
    Link { path: String, parent: String, name: String },
    Unlink { path: String },
    Rmdir { path: String },
    Remove { path: String },
    StreamWrite { path: String, stream: String, len: usize, fill: u64 },
    StreamDelete { path: String, stream: String },
    EaWrite { path: String, name: String, len: usize, fill: u64 },
    EaRemove { path: String, name: String },
}

impl Op {
    pub fn verb(&self) -> &'static str {
        match self {
            Op::Create { .. } => "create",
            Op::Mkdir { .. } => "mkdir",
            Op::Write { .. } => "write",
            Op::Replace { .. } => "replace",
            Op::WriteAt { .. } => "write_at",
            Op::Truncate { .. } => "truncate",
            Op::Rename { .. } => "rename",
            Op::Link { .. } => "link",
            Op::Unlink { .. } => "unlink",
            Op::Rmdir { .. } => "rmdir",
            Op::Remove { .. } => "remove",
            Op::StreamWrite { .. } => "stream_write",
            Op::StreamDelete { .. } => "stream_delete",
            Op::EaWrite { .. } => "ea_write",
            Op::EaRemove { .. } => "ea_remove",
        }
    }

    /// The fields after the verb, as text.
    fn fields(&self) -> Vec<String> {
        match self {
            Op::Create { parent, name } | Op::Mkdir { parent, name } => {
                vec![parent.clone(), name.clone()]
            }
            Op::Write { path, len, fill } | Op::Replace { path, len, fill } => {
                vec![path.clone(), len.to_string(), fill.to_string()]
            }
            Op::WriteAt {
                path,
                offset,
                len,
                fill,
            } => vec![
                path.clone(),
                offset.to_string(),
                len.to_string(),
                fill.to_string(),
            ],
            Op::Truncate { path, size } => vec![path.clone(), size.to_string()],
            Op::Rename {
                path,
                name,
                replace,
            } => vec![
                path.clone(),
                name.clone(),
                if *replace { "replace" } else { "keep" }.to_string(),
            ],
            Op::Link { path, parent, name } => vec![path.clone(), parent.clone(), name.clone()],
            Op::Unlink { path } | Op::Rmdir { path } | Op::Remove { path } => vec![path.clone()],
            Op::StreamWrite {
                path,
                stream,
                len,
                fill,
            } => vec![
                path.clone(),
                stream.clone(),
                len.to_string(),
                fill.to_string(),
            ],
            Op::StreamDelete { path, stream } => vec![path.clone(), stream.clone()],
            Op::EaWrite {
                path,
                name,
                len,
                fill,
            } => vec![path.clone(), name.clone(), len.to_string(), fill.to_string()],
            Op::EaRemove { path, name } => vec![path.clone(), name.clone()],
        }
    }

    /// The strings the caller chose: a refusal that quotes them is
    /// counted with every other refusal of the same shape.
    pub fn arguments(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .fields()
            .into_iter()
            .filter(|f| f.chars().any(|c| !c.is_ascii_digit()) && f != "/")
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v
    }

    /// One line of a reproducer: the verb and the fields, tab-separated.
    /// A Win32 name cannot hold a tab, so no field needs quoting.
    pub fn to_line(&self) -> String {
        let mut parts = vec![self.verb().to_string()];
        parts.extend(self.fields());
        parts.join("\t")
    }

    pub fn parse(line: &str) -> Result<Op, String> {
        let f: Vec<&str> = line.split('\t').collect();
        let s = |i: usize| -> Result<String, String> {
            f.get(i)
                .map(|x| x.to_string())
                .ok_or_else(|| format!("`{line}`: field {i} is missing"))
        };
        let n = |i: usize| -> Result<u64, String> {
            s(i)?
                .parse::<u64>()
                .map_err(|e| format!("`{line}`: field {i}: {e}"))
        };
        let op = match f[0] {
            "create" => Op::Create {
                parent: s(1)?,
                name: s(2)?,
            },
            "mkdir" => Op::Mkdir {
                parent: s(1)?,
                name: s(2)?,
            },
            "write" => Op::Write {
                path: s(1)?,
                len: n(2)? as usize,
                fill: n(3)?,
            },
            "replace" => Op::Replace {
                path: s(1)?,
                len: n(2)? as usize,
                fill: n(3)?,
            },
            "write_at" => Op::WriteAt {
                path: s(1)?,
                offset: n(2)?,
                len: n(3)? as usize,
                fill: n(4)?,
            },
            "truncate" => Op::Truncate {
                path: s(1)?,
                size: n(2)?,
            },
            "rename" => Op::Rename {
                path: s(1)?,
                name: s(2)?,
                replace: match s(3)?.as_str() {
                    "replace" => true,
                    "keep" => false,
                    other => return Err(format!("`{line}`: `{other}` is not replace or keep")),
                },
            },
            "link" => Op::Link {
                path: s(1)?,
                parent: s(2)?,
                name: s(3)?,
            },
            "unlink" => Op::Unlink { path: s(1)? },
            "rmdir" => Op::Rmdir { path: s(1)? },
            "remove" => Op::Remove { path: s(1)? },
            "stream_write" => Op::StreamWrite {
                path: s(1)?,
                stream: s(2)?,
                len: n(3)? as usize,
                fill: n(4)?,
            },
            "stream_delete" => Op::StreamDelete {
                path: s(1)?,
                stream: s(2)?,
            },
            "ea_write" => Op::EaWrite {
                path: s(1)?,
                name: s(2)?,
                len: n(3)? as usize,
                fill: n(4)?,
            },
            "ea_remove" => Op::EaRemove {
                path: s(1)?,
                name: s(2)?,
            },
            other => return Err(format!("`{line}`: `{other}` is not an operation")),
        };
        Ok(op)
    }

    /// Every path whose node the operation can change, before and after:
    /// each is compared with the model once the call returns, whatever it
    /// returned.
    fn touched(&self) -> Vec<String> {
        match self {
            Op::Create { parent, name } | Op::Mkdir { parent, name } => {
                vec![parent.clone(), join(parent, name)]
            }
            Op::Rename { path, name, .. } => {
                let parent = parent_of(path);
                vec![parent.clone(), path.clone(), join(&parent, name)]
            }
            Op::Link { path, parent, name } => {
                vec![parent_of(path), path.clone(), parent.clone(), join(parent, name)]
            }
            Op::Unlink { path } | Op::Rmdir { path } | Op::Remove { path } => {
                vec![parent_of(path), path.clone()]
            }
            Op::Write { path, .. }
            | Op::Replace { path, .. }
            | Op::WriteAt { path, .. }
            | Op::Truncate { path, .. }
            | Op::StreamWrite { path, .. }
            | Op::StreamDelete { path, .. }
            | Op::EaWrite { path, .. }
            | Op::EaRemove { path, .. } => vec![parent_of(path), path.clone()],
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_line().replace('\t', " "))
    }
}

pub fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

pub fn parent_of(path: &str) -> String {
    match path.rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
    }
}

fn base_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, b)| b)
}

/// Call the driver. The value is the record number a create returns and
/// the byte count a write returns; the other calls give 0.
fn drive(io: &mut MemIo, op: &Op) -> Result<u64, Error> {
    match op {
        Op::Create { parent, name } => write::create_file_io(io, parent, name),
        Op::Mkdir { parent, name } => write::mkdir_io(io, parent, name),
        Op::Write { path, len, fill } => {
            write::write_file_contents_io(io, path, &fill_bytes(*fill, *len))
        }
        Op::Replace { path, len, fill } => {
            write::replace_file_contents_io(io, path, &fill_bytes(*fill, *len))
        }
        Op::WriteAt {
            path,
            offset,
            len,
            fill,
        } => write::write_at_io(io, path, *offset, &fill_bytes(*fill, *len)),
        Op::Truncate { path, size } => write::truncate_io(io, path, *size),
        Op::Rename {
            path,
            name,
            replace,
        } => write::rename_replace_io(io, path, name, *replace).map(|()| 0),
        Op::Link { path, parent, name } => write::link_io(io, path, parent, name).map(|()| 0),
        Op::Unlink { path } => write::unlink_io(io, path).map(|()| 0),
        Op::Rmdir { path } => write::rmdir_io(io, path).map(|()| 0),
        Op::Remove { path } => write::remove_io(io, path).map(|()| 0),
        Op::StreamWrite {
            path,
            stream,
            len,
            fill,
        } => write::write_named_stream_io(io, path, stream, &fill_bytes(*fill, *len)).map(|()| 0),
        Op::StreamDelete { path, stream } => {
            write::delete_named_stream_io(io, path, stream).map(|()| 0)
        }
        Op::EaWrite {
            path,
            name,
            len,
            fill,
        } => write::write_ea_io(io, path, name.as_bytes(), &fill_bytes(*fill, *len), 0).map(|()| 0),
        Op::EaRemove { path, name } => write::remove_ea_io(io, path, name.as_bytes()).map(|()| 0),
    }
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

pub type NodeId = usize;
pub const ROOT: NodeId = 0;

/// Longest name Windows accepts, in UTF-16 code units.
const MAX_NAME_UNITS: usize = 255;

pub struct Entry {
    /// The spelling the entry was created or renamed with.
    pub name: String,
    pub node: NodeId,
}

pub enum NodeKind {
    File(Vec<u8>),
    /// Keyed by the name folded through the volume's `$UpCase`, which is
    /// what makes two names the same name on NTFS.
    Dir(BTreeMap<Vec<u16>, Entry>),
}

pub struct Node {
    pub record: u64,
    pub kind: NodeKind,
    /// Named streams, keyed by the folded name; the value keeps the
    /// spelling the stream was created with, as Windows does (#494).
    pub streams: BTreeMap<Vec<u16>, (String, Vec<u8>)>,
    /// Extended attributes, keyed by the name as stored: ASCII letters
    /// upper-cased (#482).
    pub eas: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Directory entries naming the node.
    pub links: usize,
}

impl Node {
    fn new(record: u64, kind: NodeKind) -> Self {
        Node {
            record,
            kind,
            streams: BTreeMap::new(),
            eas: BTreeMap::new(),
            links: 1,
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, NodeKind::Dir(_))
    }
}

/// What the model says a call must do. Where the model has no opinion
/// the driver may refuse -- a feature it does not have yet is a refusal,
/// counted in the histogram -- but whatever it accepts is applied to the
/// model and then compared.
pub enum Expect {
    Refuse(String),
    Either,
}

pub struct Model {
    pub nodes: BTreeMap<NodeId, Node>,
    next: NodeId,
    upcase: UpcaseTable,
}

impl Model {
    pub fn new(upcase: UpcaseTable, root_record: u64) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, Node::new(root_record, NodeKind::Dir(BTreeMap::new())));
        Model {
            nodes,
            next: ROOT + 1,
            upcase,
        }
    }

    pub fn fold(&self, name: &str) -> Vec<u16> {
        name.encode_utf16().map(|u| self.upcase.upcase(u)).collect()
    }

    pub fn cmp_names(&self, a: &str, b: &str) -> std::cmp::Ordering {
        let a: Vec<u16> = a.encode_utf16().collect();
        let b: Vec<u16> = b.encode_utf16().collect();
        self.upcase.cmp_names(&a, &b)
    }

    /// The path with every component upper-cased through `$UpCase`: the
    /// same path to a case-insensitive lookup.
    pub fn upcased(&self, path: &str) -> String {
        if path == "/" {
            return "/".to_string();
        }
        path.split('/')
            .map(|c| String::from_utf16_lossy(&self.fold(c)))
            .collect::<Vec<_>>()
            .join("/")
    }

    fn entries(&self, dir: NodeId) -> Option<&BTreeMap<Vec<u16>, Entry>> {
        match &self.nodes.get(&dir)?.kind {
            NodeKind::Dir(entries) => Some(entries),
            NodeKind::File(_) => None,
        }
    }

    fn entries_mut(&mut self, dir: NodeId) -> &mut BTreeMap<Vec<u16>, Entry> {
        match &mut self.nodes.get_mut(&dir).expect("directory in the model").kind {
            NodeKind::Dir(entries) => entries,
            NodeKind::File(_) => panic!("model: node {dir} is not a directory"),
        }
    }

    pub fn lookup(&self, path: &str) -> Option<NodeId> {
        let mut at = ROOT;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            at = self.entries(at)?.get(&self.fold(c))?.node;
        }
        Some(at)
    }

    /// The directory holding `path`'s last component, and that component
    /// folded. `None` when the parent does not exist or is a file.
    fn lookup_entry(&self, path: &str) -> Option<(NodeId, Vec<u16>)> {
        if path == "/" {
            return None;
        }
        let parent = self.lookup(&parent_of(path))?;
        self.entries(parent)?;
        Some((parent, self.fold(base_of(path))))
    }

    /// Every path in the tree with its node, root first. A file with
    /// several names appears once per name.
    pub fn paths(&self) -> Vec<(String, NodeId)> {
        let mut out = vec![("/".to_string(), ROOT)];
        let mut i = 0;
        while i < out.len() {
            let (path, id) = (out[i].0.clone(), out[i].1);
            if let Some(entries) = self.entries(id) {
                for e in entries.values() {
                    out.push((join(&path, &e.name), e.node));
                }
            }
            i += 1;
        }
        out
    }

    fn new_node(&mut self, record: u64, kind: NodeKind) -> NodeId {
        let id = self.next;
        self.next += 1;
        self.nodes.insert(id, Node::new(record, kind));
        id
    }

    /// One name fewer for `id`; the node goes with its last one.
    fn drop_link(&mut self, id: NodeId) {
        let node = self.nodes.get_mut(&id).expect("node in the model");
        node.links -= 1;
        if node.links == 0 {
            self.nodes.remove(&id);
        }
    }

    fn valid_name(name: &str) -> Result<(), String> {
        let units = name.encode_utf16().count();
        if units == 0 {
            return Err("the name is empty".to_string());
        }
        if units > MAX_NAME_UNITS {
            return Err(format!("the name is {units} UTF-16 units, over Windows' 255"));
        }
        Ok(())
    }

    pub fn expect(&self, op: &Op) -> Expect {
        match self.precondition(op) {
            Ok(()) => Expect::Either,
            Err(why) => Expect::Refuse(why),
        }
    }

    fn file_at(&self, path: &str) -> Result<NodeId, String> {
        let id = self.existing(path)?;
        if self.nodes[&id].is_dir() {
            return Err(format!("{path} is a directory"));
        }
        Ok(id)
    }

    fn dir_at(&self, path: &str) -> Result<NodeId, String> {
        let id = self.existing(path)?;
        if !self.nodes[&id].is_dir() {
            return Err(format!("{path} is not a directory"));
        }
        Ok(id)
    }

    fn existing(&self, path: &str) -> Result<NodeId, String> {
        self.lookup(path)
            .ok_or_else(|| format!("{path} does not exist"))
    }

    fn not_root(path: &str) -> Result<(), String> {
        if path == "/" {
            return Err("it names the root directory".to_string());
        }
        Ok(())
    }

    fn free_name(&self, parent: NodeId, name: &str) -> Result<(), String> {
        Self::valid_name(name)?;
        if self.entries(parent).is_some_and(|e| e.contains_key(&self.fold(name))) {
            return Err(format!("{name} is taken in its directory"));
        }
        Ok(())
    }

    fn precondition(&self, op: &Op) -> Result<(), String> {
        match op {
            Op::Create { parent, name } | Op::Mkdir { parent, name } => {
                let p = self.dir_at(parent)?;
                self.free_name(p, name)
            }
            Op::Write { path, .. }
            | Op::Replace { path, .. }
            | Op::WriteAt { path, .. }
            | Op::Truncate { path, .. } => self.file_at(path).map(|_| ()),
            Op::Rename {
                path,
                name,
                replace,
            } => {
                Self::not_root(path)?;
                let src = self.existing(path)?;
                Self::valid_name(name)?;
                let (parent, old_key) = self.lookup_entry(path).expect("an existing path");
                let new_key = self.fold(name);
                if new_key == old_key {
                    return Ok(());
                }
                let Some(dest) = self.entries(parent).and_then(|e| e.get(&new_key)) else {
                    return Ok(());
                };
                if !replace {
                    return Err(format!("{name} is taken and the rename does not replace"));
                }
                let (s, d) = (&self.nodes[&src], &self.nodes[&dest.node]);
                match (s.is_dir(), &d.kind) {
                    (true, NodeKind::File(_)) => Err(format!("{name} is a file and {path} a directory")),
                    (false, NodeKind::Dir(_)) => Err(format!("{name} is a directory and {path} a file")),
                    (true, NodeKind::Dir(e)) if !e.is_empty() => {
                        Err(format!("{name} is a directory that is not empty"))
                    }
                    _ => Ok(()),
                }
            }
            Op::Link { path, parent, name } => {
                self.file_at(path)?;
                let p = self.dir_at(parent)?;
                self.free_name(p, name)
            }
            Op::Unlink { path } => self.file_at(path).map(|_| ()),
            Op::Rmdir { path } => {
                Self::not_root(path)?;
                let d = self.dir_at(path)?;
                if self.entries(d).is_some_and(|e| !e.is_empty()) {
                    return Err(format!("{path} is not empty"));
                }
                Ok(())
            }
            Op::Remove { path } => {
                Self::not_root(path)?;
                let id = self.existing(path)?;
                if self.entries(id).is_some_and(|e| !e.is_empty()) {
                    return Err(format!("{path} is a directory that is not empty"));
                }
                Ok(())
            }
            Op::StreamWrite { path, .. } | Op::EaWrite { path, .. } => {
                Self::not_root(path)?;
                self.existing(path).map(|_| ())
            }
            Op::StreamDelete { path, stream } => {
                Self::not_root(path)?;
                let id = self.existing(path)?;
                if !self.nodes[&id].streams.contains_key(&self.fold(stream)) {
                    return Err(format!("{path} has no stream {stream}"));
                }
                Ok(())
            }
            Op::EaRemove { path, name } => {
                Self::not_root(path)?;
                let id = self.existing(path)?;
                if !self.nodes[&id].eas.contains_key(&ea_key(name)) {
                    return Err(format!("{path} has no extended attribute {name}"));
                }
                Ok(())
            }
        }
    }

    /// Apply an operation the driver accepted. `value` is what the call
    /// returned. An `Err` is a result the model cannot make sense of.
    pub fn apply(&mut self, op: &Op, value: u64) -> Result<(), String> {
        match op {
            Op::Create { parent, name } | Op::Mkdir { parent, name } => {
                let p = self.lookup(parent).expect("checked");
                let kind = if matches!(op, Op::Create { .. }) {
                    NodeKind::File(Vec::new())
                } else {
                    NodeKind::Dir(BTreeMap::new())
                };
                if let Some(other) = self.nodes.values().find(|n| n.record == value) {
                    return Err(format!(
                        "the new entry was given record {value}, which {} in the model already has",
                        if other.is_dir() { "a directory" } else { "a file" }
                    ));
                }
                let id = self.new_node(value, kind);
                let key = self.fold(name);
                self.entries_mut(p).insert(
                    key,
                    Entry {
                        name: name.clone(),
                        node: id,
                    },
                );
            }
            Op::Write { path, len, fill } | Op::Replace { path, len, fill } => {
                *self.data_mut(path) = fill_bytes(*fill, *len);
            }
            Op::WriteAt {
                path,
                offset,
                len,
                fill,
            } => {
                let n = usize::try_from(value).unwrap_or(usize::MAX);
                if n > *len {
                    return Err(format!("write_at reported {value} bytes written of {len}"));
                }
                let bytes = fill_bytes(*fill, n);
                let off = *offset as usize;
                let data = self.data_mut(path);
                if data.len() < off + n {
                    data.resize(off + n, 0);
                }
                data[off..off + n].copy_from_slice(&bytes);
            }
            Op::Truncate { path, size } => {
                self.data_mut(path).resize(*size as usize, 0);
            }
            Op::Rename { path, name, .. } => {
                let src = self.lookup(path).expect("checked");
                let (parent, old_key) = self.lookup_entry(path).expect("checked");
                let new_key = self.fold(name);
                if new_key == old_key {
                    self.entries_mut(parent)
                        .get_mut(&old_key)
                        .expect("checked")
                        .name = name.clone();
                    return Ok(());
                }
                if let Some(dest) = self.entries(parent).and_then(|e| e.get(&new_key)) {
                    let dest = dest.node;
                    if dest == src {
                        // Two names of one file: POSIX leaves both.
                        return Ok(());
                    }
                    self.entries_mut(parent).remove(&new_key);
                    self.drop_link(dest);
                }
                let entries = self.entries_mut(parent);
                let e = entries.remove(&old_key).expect("checked");
                entries.insert(
                    new_key,
                    Entry {
                        name: name.clone(),
                        node: e.node,
                    },
                );
            }
            Op::Link { path, parent, name } => {
                let id = self.lookup(path).expect("checked");
                let p = self.lookup(parent).expect("checked");
                let key = self.fold(name);
                self.entries_mut(p).insert(
                    key,
                    Entry {
                        name: name.clone(),
                        node: id,
                    },
                );
                self.nodes.get_mut(&id).expect("checked").links += 1;
            }
            Op::Unlink { path } | Op::Rmdir { path } | Op::Remove { path } => {
                let (parent, key) = self.lookup_entry(path).expect("checked");
                let e = self.entries_mut(parent).remove(&key).expect("checked");
                self.drop_link(e.node);
            }
            Op::StreamWrite {
                path,
                stream,
                len,
                fill,
            } => {
                let key = self.fold(stream);
                let id = self.lookup(path).expect("checked");
                let streams = &mut self.nodes.get_mut(&id).expect("checked").streams;
                let data = fill_bytes(*fill, *len);
                match streams.get_mut(&key) {
                    Some(s) => s.1 = data,
                    None => {
                        streams.insert(key, (stream.clone(), data));
                    }
                }
            }
            Op::StreamDelete { path, stream } => {
                let key = self.fold(stream);
                let id = self.lookup(path).expect("checked");
                self.nodes.get_mut(&id).expect("checked").streams.remove(&key);
            }
            Op::EaWrite {
                path,
                name,
                len,
                fill,
            } => {
                let id = self.lookup(path).expect("checked");
                self.nodes
                    .get_mut(&id)
                    .expect("checked")
                    .eas
                    .insert(ea_key(name), fill_bytes(*fill, *len));
            }
            Op::EaRemove { path, name } => {
                let id = self.lookup(path).expect("checked");
                self.nodes.get_mut(&id).expect("checked").eas.remove(&ea_key(name));
            }
        }
        Ok(())
    }

    fn data_mut(&mut self, path: &str) -> &mut Vec<u8> {
        let id = self.lookup(path).expect("checked");
        match &mut self.nodes.get_mut(&id).expect("checked").kind {
            NodeKind::File(data) => data,
            NodeKind::Dir(_) => panic!("model: {path} is a directory"),
        }
    }
}

/// An extended attribute's name as NTFS stores it: ASCII letters
/// upper-cased, everything else as given.
fn ea_key(name: &str) -> Vec<u8> {
    name.as_bytes().to_ascii_uppercase()
}

// ---------------------------------------------------------------------------
// The generator
// ---------------------------------------------------------------------------

/// Names built to collide: case pairs, Latin-1 and Greek letters whose
/// upper case `$UpCase` and Rust disagree on, names at and over the
/// 255-unit limit, CJK, and characters outside the BMP.
fn name_pool() -> Vec<String> {
    let mut v: Vec<String> = [
        "a", "A", "b", "B", "abc", "ABC", "Abc", "abd", "file.txt", "FILE.TXT", "File.Txt", "z",
        "Z", "readme", "README", "x1", "X1", "x10", "x2", "é", "É", "e", "E", "ÿ", "Ÿ", "ß", "SS",
        "ss", "µ", "Μ", "μ", "σ", "Σ", "ς", "αβγ", "ΑΒΓ", "Ωmega", "ωMEGA", "ı", "I", "i", "İ",
        "café", "CAFÉ", "naïve", "NAÏVE", "日本語", "テスト", "中文名", "한국어", "😀", "😀x",
        "a😀", "🎉🎉", "ǅ", "ǆ", "Ǆ", "dir", "DIR", "sub", "a.b.c", "A.B.C", "with space",
        "WITH SPACE", "-dash", "_under", "~tilde", "many.dots.in.a.name",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push("n".repeat(255));
    v.push("N".repeat(255));
    v.push("n".repeat(254));
    v.push("é".repeat(255));
    v.push("😀".repeat(127) + "x");
    // Over the limit: Windows refuses both.
    v.push("o".repeat(256));
    v.push("😀".repeat(128));
    v
}

fn stream_pool() -> Vec<String> {
    let mut v: Vec<String> = [
        "s", "S", "zz", "ZZ", "aa", "Zone.Identifier", "zone.identifier", "σ", "Σ", "流れ", "😀",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push("x".repeat(255));
    v
}

fn ea_pool() -> Vec<String> {
    let mut v: Vec<String> = ["A", "a", "user.a", "USER.A", "user.b", "X", "dosattrib"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.push("E".repeat(200));
    v
}

pub struct Generator {
    rng: Rng,
    names: Vec<String>,
    streams: Vec<String>,
    eas: Vec<String>,
    /// A sequence that mostly creates, with fresh names, so directories
    /// grow past their resident root into index blocks.
    wide: bool,
    fresh: u64,
}

impl Generator {
    pub fn new(rng: Rng) -> Self {
        let mut rng = rng;
        let wide = rng.chance(30);
        Generator {
            rng,
            names: name_pool(),
            streams: stream_pool(),
            eas: ea_pool(),
            wide,
            fresh: 0,
        }
    }

    fn data_len(&mut self) -> usize {
        let r = &mut self.rng;
        (match r.below(100) {
            0..=9 => 0,
            10..=29 => r.range(1, 64),
            30..=54 => r.range(65, 900),
            55..=74 => r.range(901, 8192),
            75..=94 => r.range(8193, 128 * 1024),
            _ => r.range(128 * 1024 + 1, 1536 * 1024),
        }) as usize
    }

    fn small_len(&mut self) -> usize {
        let r = &mut self.rng;
        (match r.below(100) {
            0..=29 => r.range(1, 64),
            30..=69 => r.range(65, 4096),
            70..=94 => r.range(4097, 65536),
            _ => r.range(65537, 512 * 1024),
        }) as usize
    }

    fn fill(&mut self) -> u64 {
        self.rng.next_u64()
    }

    /// A name for a new entry in `dir`: from the pool, a case variant of
    /// a name already there, or a fresh one.
    fn new_name(&mut self, model: &Model, dir: &str) -> String {
        let fresh_percent = if self.wide { 50 } else { 15 };
        let r = self.rng.below(100);
        if r < fresh_percent {
            self.fresh += 1;
            // Long fresh names fill an index block sooner.
            return if self.rng.chance(30) {
                format!("{:0>120}", self.fresh)
            } else {
                format!("f{}", self.fresh)
            };
        }
        if r < fresh_percent + 15 {
            let siblings: Vec<String> = model
                .lookup(dir)
                .and_then(|d| model.entries(d))
                .map(|e| e.values().map(|e| e.name.clone()).collect())
                .unwrap_or_default();
            if !siblings.is_empty() {
                let s = self.rng.pick(&siblings).clone();
                return self.case_variant(&s);
            }
        }
        self.rng.pick(&self.names).clone()
    }

    fn case_variant(&mut self, name: &str) -> String {
        match self.rng.below(3) {
            0 => name.to_uppercase(),
            1 => name.to_lowercase(),
            _ => name
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if i % 2 == 0 {
                        c.to_uppercase().next().unwrap_or(c)
                    } else {
                        c
                    }
                })
                .collect(),
        }
    }

    /// The path the call is given for an entry: usually as spelled,
    /// sometimes upper-cased (a lookup is case-insensitive), now and then
    /// one that does not exist.
    fn address(&mut self, model: &Model, path: &str) -> String {
        match self.rng.below(100) {
            0..=3 => join(&parent_of(path), "nothere"),
            4..=19 => model.upcased(path),
            _ => path.to_string(),
        }
    }

    fn pick_path(&mut self, model: &Model, want: Option<bool>) -> Option<String> {
        let paths: Vec<String> = model
            .paths()
            .into_iter()
            .filter(|(p, id)| p != "/" && want.is_none_or(|dir| model.nodes[id].is_dir() == dir))
            .map(|(p, _)| p)
            .collect();
        if paths.is_empty() {
            return None;
        }
        let p = self.rng.pick(&paths).clone();
        Some(self.address(model, &p))
    }

    fn pick_dir(&mut self, model: &Model) -> String {
        if self.rng.chance(35) {
            return "/".to_string();
        }
        let dirs: Vec<String> = model
            .paths()
            .into_iter()
            .filter(|(_, id)| model.nodes[id].is_dir())
            .map(|(p, _)| p)
            .collect();
        let d = self.rng.pick(&dirs).clone();
        self.address(model, &d)
    }

    /// A file, now and then a directory, so the refusal is exercised too.
    fn pick_file(&mut self, model: &Model) -> Option<String> {
        if self.rng.chance(4) {
            if let Some(d) = self.pick_path(model, Some(true)) {
                return Some(d);
            }
        }
        self.pick_path(model, Some(false))
    }

    fn create(&mut self, model: &Model, dir: bool) -> Op {
        let parent = self.pick_dir(model);
        let name = self.new_name(model, &parent);
        if dir {
            Op::Mkdir { parent, name }
        } else {
            Op::Create { parent, name }
        }
    }

    fn size_of(model: &Model, path: &str) -> u64 {
        match model.lookup(path).map(|id| &model.nodes[&id].kind) {
            Some(NodeKind::File(d)) => d.len() as u64,
            _ => 0,
        }
    }

    pub fn next_op(&mut self, model: &Model) -> Op {
        let roll = self.rng.below(100);
        let roll = if self.wide && roll >= 60 && self.rng.chance(50) {
            0
        } else {
            roll
        };
        let op = match roll {
            0..=13 => Some(self.create(model, false)),
            14..=19 => Some(self.create(model, true)),
            20..=31 => self.pick_file(model).map(|path| {
                let (len, fill) = (self.data_len(), self.fill());
                Op::Write { path, len, fill }
            }),
            32..=41 => self.pick_file(model).map(|path| {
                let size = Self::size_of(model, &path);
                let offset = match self.rng.below(6) {
                    0 => 0,
                    1 => self.rng.range(0, size),
                    2 => size,
                    3 => size + self.rng.range(1, 65536),
                    4 => (size / 4096) * 4096,
                    _ => self.rng.range(0, size + 4096) | 0x1ff,
                };
                let (len, fill) = (self.small_len(), self.fill());
                Op::WriteAt {
                    path,
                    offset,
                    len,
                    fill,
                }
            }),
            42..=47 => self.pick_file(model).map(|path| {
                let cur = Self::size_of(model, &path);
                let size = match self.rng.below(6) {
                    0 => 0,
                    1 => cur / 2,
                    2 => cur.saturating_sub(1),
                    3 => cur + self.rng.range(1, 65536),
                    4 => cur,
                    _ => self.rng.range(0, 256 * 1024),
                };
                Op::Truncate { path, size }
            }),
            48..=50 => self.pick_file(model).map(|path| {
                let (len, fill) = (self.data_len(), self.fill());
                Op::Replace { path, len, fill }
            }),
            51..=58 => self.pick_path(model, None).map(|path| {
                let parent = parent_of(&path);
                let own = base_of(&path).to_string();
                let name = match self.rng.below(10) {
                    0..=2 => self.case_variant(&own),
                    3..=4 => {
                        // Same length, different name (#466).
                        let n = own.chars().count();
                        let mut s: String = own.chars().take(n.saturating_sub(1)).collect();
                        s.push(if own.ends_with('q') { 'r' } else { 'q' });
                        s
                    }
                    _ => self.new_name(model, &parent),
                };
                let replace = self.rng.chance(35);
                // Two names of one file in one directory: what a replacing
                // rename does there is unsettled, so it is not asked.
                let same_file = model.lookup(&path).is_some()
                    && model.lookup(&path) == model.lookup(&join(&parent, &name))
                    && model.fold(&own) != model.fold(&name);
                Op::Rename {
                    path,
                    name,
                    replace: replace && !same_file,
                }
            }),
            59..=63 => self.pick_file(model).map(|path| {
                let parent = self.pick_dir(model);
                let name = self.new_name(model, &parent);
                Op::Link { path, parent, name }
            }),
            64..=69 => self.pick_file(model).map(|path| Op::Unlink { path }),
            70..=72 => {
                let want_dir = !self.rng.chance(10);
                self.pick_path(model, Some(want_dir))
                    .map(|path| Op::Rmdir { path })
            }
            73..=75 => self.pick_path(model, None).map(|path| Op::Remove { path }),
            76..=84 => self.pick_path(model, Some(false).filter(|_| self.rng.chance(85))).map(|path| {
                let stream = self.rng.pick(&self.streams).clone();
                let len = if self.rng.chance(50) {
                    self.small_len()
                } else {
                    self.data_len()
                };
                let fill = self.fill();
                Op::StreamWrite {
                    path,
                    stream,
                    len,
                    fill,
                }
            }),
            85..=87 => self.pick_path(model, None).map(|path| {
                let existing: Vec<String> = model
                    .lookup(&path)
                    .map(|id| model.nodes[&id].streams.values().map(|s| s.0.clone()).collect())
                    .unwrap_or_default();
                let stream = if !existing.is_empty() && self.rng.chance(80) {
                    let s = self.rng.pick(&existing).clone();
                    if self.rng.chance(25) {
                        self.case_variant(&s)
                    } else {
                        s
                    }
                } else {
                    self.rng.pick(&self.streams).clone()
                };
                Op::StreamDelete { path, stream }
            }),
            88..=94 => self.pick_path(model, None).map(|path| {
                let name = self.rng.pick(&self.eas).clone();
                let len = match self.rng.below(10) {
                    0..=4 => self.rng.range(0, 64),
                    5..=8 => self.rng.range(65, 500),
                    _ => self.rng.range(501, 4000),
                } as usize;
                let fill = self.fill();
                Op::EaWrite {
                    path,
                    name,
                    len,
                    fill,
                }
            }),
            _ => self.pick_path(model, None).map(|path| {
                let existing: Vec<String> = model
                    .lookup(&path)
                    .map(|id| {
                        model.nodes[&id]
                            .eas
                            .keys()
                            .map(|k| String::from_utf8_lossy(k).into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                let name = if !existing.is_empty() && self.rng.chance(80) {
                    let s = self.rng.pick(&existing).clone();
                    s.to_lowercase()
                } else {
                    self.rng.pick(&self.eas).clone()
                };
                Op::EaRemove { path, name }
            }),
        };
        op.unwrap_or_else(|| self.create(model, self.rng.chance(20)))
    }
}

// ---------------------------------------------------------------------------
// Comparing a volume with the model
// ---------------------------------------------------------------------------

/// A difference between the volume and the model. `class` names its
/// kind, so a shrunk reproducer can be held to the same failure.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub class: String,
    pub detail: String,
}

pub fn mismatch(class: impl Into<String>, detail: impl Into<String>) -> Mismatch {
    Mismatch {
        class: class.into(),
        detail: detail.into(),
    }
}

/// Where two byte strings first differ, for a message.
pub fn describe_bytes(got: &[u8], want: &[u8]) -> String {
    let first = got.iter().zip(want).position(|(a, b)| a != b);
    match first {
        Some(i) => format!(
            "{} bytes read, {} expected, first difference at byte {i} (read {:#04x}, expected {:#04x})",
            got.len(),
            want.len(),
            got[i],
            want[i]
        ),
        None => format!(
            "{} bytes read, {} expected, equal over the shorter",
            got.len(),
            want.len()
        ),
    }
}

/// Compare `path` with the model through this crate's read layer: the
/// node there, or the absence of one.
fn verify_path(io: &mut MemIo, model: &Model, path: &str) -> Result<(), Mismatch> {
    match model.lookup(path) {
        Some(id) => verify_node(io, model, id, path),
        None => match read::resolve_path(io, path) {
            Ok(rec) => Err(mismatch(
                "ghost",
                format!("{path} resolves to record {rec}, and the model has nothing there"),
            )),
            Err(e) if matches!(e.kind(), Kind::NotFound | Kind::NotDirectory) => Ok(()),
            Err(e) => Err(mismatch(
                "lookup",
                format!("looking up {path}, which the model does not have, failed: {e}"),
            )),
        },
    }
}

fn verify_node(io: &mut MemIo, model: &Model, id: NodeId, path: &str) -> Result<(), Mismatch> {
    let node = &model.nodes[&id];
    let rec = read::resolve_path(io, path)
        .map_err(|e| mismatch("lookup", format!("{path} does not resolve: {e}")))?;
    if rec != node.record {
        return Err(mismatch(
            "lookup",
            format!("{path} resolves to record {rec}, the model's is {}", node.record),
        ));
    }
    let upper = model.upcased(path);
    if upper != path {
        let r = read::resolve_path(io, &upper)
            .map_err(|e| mismatch("lookup-case", format!("{upper} does not resolve: {e}")))?;
        if r != rec {
            return Err(mismatch(
                "lookup-case",
                format!("{upper} resolves to record {r}, {path} to {rec}"),
            ));
        }
    }
    let st = read::read_stat(io, rec)
        .map_err(|e| mismatch("stat", format!("{path} (record {rec}): {e}")))?;
    if st.is_dir != node.is_dir() {
        return Err(mismatch(
            "type",
            format!("{path}: the volume has is_dir {}, the model {}", st.is_dir, node.is_dir()),
        ));
    }
    match &node.kind {
        NodeKind::File(data) => {
            if st.size != data.len() as u64 {
                return Err(mismatch(
                    "size",
                    format!("{path}: size {} on the volume, {} in the model", st.size, data.len()),
                ));
            }
            if usize::from(st.link_count) != node.links {
                return Err(mismatch(
                    "links",
                    format!(
                        "{path}: link count {} on the volume, {} names in the model",
                        st.link_count, node.links
                    ),
                ));
            }
            let got = read::read_attribute_value(io, rec, AttrType::Data, None)
                .map_err(|e| mismatch("data", format!("{path}: reading $DATA: {e}")))?;
            if got != *data {
                return Err(mismatch(
                    "data",
                    format!("{path}: {}", describe_bytes(&got, data)),
                ));
            }
        }
        NodeKind::Dir(entries) => {
            let mut listed: Vec<(String, u64, bool)> = read::read_dir_entries(io, rec)
                .map_err(|e| mismatch("listing", format!("{path}: listing failed: {e}")))?
                .into_iter()
                .filter(read::listed_to_hosts)
                .map(|e| (e.name, e.record_number, e.is_dir))
                .collect();
            listed.sort();
            let mut want: Vec<(String, u64, bool)> = entries
                .values()
                .map(|e| {
                    let n = &model.nodes[&e.node];
                    (e.name.clone(), n.record, n.is_dir())
                })
                .collect();
            want.sort();
            if listed != want {
                return Err(mismatch(
                    "listing",
                    format!("{path}: {}", describe_listing(&listed, &want)),
                ));
            }
        }
    }

    let names = write::list_named_streams_io(io, path)
        .map_err(|e| mismatch("streams", format!("{path}: listing streams failed: {e}")))?;
    if let Some(w) = names
        .windows(2)
        .find(|w| model.cmp_names(&w[0], &w[1]) != std::cmp::Ordering::Less)
    {
        return Err(mismatch(
            "stream-order",
            format!(
                "{path}: stream {:?} is stored before {:?}, out of collation order (#467)",
                w[0], w[1]
            ),
        ));
    }
    let mut got_names = names;
    got_names.sort();
    let mut want_names: Vec<String> = node.streams.values().map(|s| s.0.clone()).collect();
    want_names.sort();
    if got_names != want_names {
        return Err(mismatch(
            "streams",
            format!("{path}: streams {got_names:?} on the volume, {want_names:?} in the model"),
        ));
    }
    for (name, data) in node.streams.values() {
        let got = read::read_attribute_value_if_present(io, rec, AttrType::Data, Some(name))
            .map_err(|e| mismatch("stream-data", format!("{path}:{name}: {e}")))?;
        match got {
            Some(got) if got == *data => {}
            Some(got) => {
                return Err(mismatch(
                    "stream-data",
                    format!("{path}:{name}: {}", describe_bytes(&got, data)),
                ))
            }
            None => {
                return Err(mismatch(
                    "stream-data",
                    format!("{path}:{name} is listed and does not read"),
                ))
            }
        }
    }

    let mut eas: Vec<(Vec<u8>, Vec<u8>)> = write::list_eas_io(io, path)
        .map_err(|e| mismatch("eas", format!("{path}: listing extended attributes failed: {e}")))?
        .into_iter()
        .map(|ea| (ea.name, ea.value))
        .collect();
    eas.sort();
    let want: Vec<(Vec<u8>, Vec<u8>)> =
        node.eas.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if eas != want {
        let show = |v: &[(Vec<u8>, Vec<u8>)]| -> Vec<String> {
            v.iter()
                .map(|(k, v)| format!("{}={}B", String::from_utf8_lossy(k), v.len()))
                .collect()
        };
        return Err(mismatch(
            "eas",
            format!(
                "{path}: extended attributes {:?} on the volume, {:?} in the model{}",
                show(&eas),
                show(&want),
                if show(&eas) == show(&want) {
                    " (the values differ)"
                } else {
                    ""
                }
            ),
        ));
    }
    Ok(())
}

fn describe_listing(listed: &[(String, u64, bool)], want: &[(String, u64, bool)]) -> String {
    let extra: Vec<String> = listed
        .iter()
        .filter(|e| !want.contains(e))
        .map(|(n, r, d)| format!("{n:?} (record {r}{})", if *d { ", dir" } else { "" }))
        .collect();
    let missing: Vec<String> = want
        .iter()
        .filter(|e| !listed.contains(e))
        .map(|(n, r, d)| format!("{n:?} (record {r}{})", if *d { ", dir" } else { "" }))
        .collect();
    format!("listed but not in the model: {extra:?}; in the model but not listed: {missing:?}")
}

/// Every node, through every name, then `check_io`.
pub fn verify_all(io: &mut MemIo, model: &Model) -> Result<(), Mismatch> {
    for (path, id) in model.paths() {
        verify_node(io, model, id, &path)?;
    }
    let report =
        fsck::check_io(io).map_err(|e| mismatch("check_io", format!("check_io failed: {e}")))?;
    if !report.findings.is_empty() {
        let shown: Vec<String> = report
            .findings
            .iter()
            .take(5)
            .map(|f| format!("{f:?}"))
            .collect();
        return Err(mismatch(
            "check_io",
            format!(
                "check_io found {} problem(s): {}",
                report.findings.len(),
                shown.join("; ")
            ),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Running a sequence
// ---------------------------------------------------------------------------

/// How many refusals of each shape, and how many operations ran.
#[derive(Default, Clone)]
pub struct Stats {
    pub sequences: u64,
    pub ops: u64,
    pub accepted: u64,
    /// (verb, refused where the model said it must be, kind, message).
    pub refusals: BTreeMap<(String, bool, String, String), u64>,
}

impl Stats {
    fn refusal(&mut self, op: &Op, expected: bool, e: &Error) {
        let key = (
            op.verb().to_string(),
            expected,
            format!("{:?}", e.kind()),
            normalize(e.message(), op),
        );
        *self.refusals.entry(key).or_default() += 1;
    }

    pub fn merge(&mut self, other: &Stats) {
        self.sequences += other.sequences;
        self.ops += other.ops;
        self.accepted += other.accepted;
        for (k, v) in &other.refusals {
            *self.refusals.entry(k.clone()).or_default() += v;
        }
    }
}

/// A refusal message with the caller's own strings and every number taken
/// out, so refusals of one shape count together.
fn normalize(message: &str, op: &Op) -> String {
    let mut m = message.to_string();
    for a in op.arguments() {
        m = m.replace(&a, "<arg>");
    }
    let mut out = String::with_capacity(m.len());
    let mut in_digits = false;
    for c in m.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                out.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            out.push(match c {
                '\t' | '\n' | '\r' => ' ',
                c => c,
            });
        }
    }
    out.chars().take(200).collect()
}

pub struct Config {
    pub ops: usize,
    pub checkpoint_every: usize,
    /// Wall-clock allowance for one sequence. A sequence that takes longer
    /// fails, naming the operation it had reached: a slow path is found
    /// rather than the tier quietly growing.
    pub deadline: Duration,
}

/// A sequence that ended with the volume and the model apart.
pub struct Failure {
    pub geometry: Geometry,
    /// Every operation run, the one that failed last.
    pub ops: Vec<Op>,
    pub class: String,
    pub what: String,
}

impl Failure {
    /// A reproducer: the geometry, then one operation a line.
    pub fn reproducer(&self, header: &str) -> String {
        let mut s = String::new();
        for line in header.lines() {
            s.push_str("# ");
            s.push_str(line);
            s.push('\n');
        }
        s.push_str(&format!(
            "geometry\t{}\t{}\t{}\n",
            self.geometry.size, self.geometry.cluster, self.geometry.record
        ));
        for op in &self.ops {
            s.push_str(&op.to_line());
            s.push('\n');
        }
        s
    }
}

/// A parsed reproducer.
pub struct Script {
    pub geometry: Geometry,
    pub ops: Vec<Op>,
    /// The issue a `# known-defect: #N` line names.
    pub known_defect: Option<String>,
}

pub fn parse_script(text: &str) -> Result<Script, String> {
    let mut geometry = None;
    let mut ops = Vec::new();
    let mut known_defect = None;
    for line in text.lines() {
        if let Some(c) = line.strip_prefix('#') {
            if let Some(issue) = c.trim().strip_prefix("known-defect:") {
                known_defect = Some(issue.trim().to_string());
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(g) = line.strip_prefix("geometry\t") {
            let n: Vec<u64> = g
                .split('\t')
                .map(|x| x.parse::<u64>().map_err(|e| format!("`{line}`: {e}")))
                .collect::<Result<_, _>>()?;
            let [size, cluster, record] = n[..] else {
                return Err(format!("`{line}`: geometry takes size, cluster and record"));
            };
            geometry = Some(Geometry {
                size,
                cluster: cluster as u32,
                record: record as u32,
            });
            continue;
        }
        ops.push(Op::parse(line)?);
    }
    Ok(Script {
        geometry: geometry.ok_or("no geometry line")?,
        ops,
        known_defect,
    })
}

/// What the volume came to after a sequence that held: its bytes and the
/// model, for the independent readers.
pub struct Finished {
    pub io: MemIo,
    pub model: Model,
}

/// Format a volume and run operations on it and on the model, comparing
/// after every one. `next` gives the operations: a generator, or a
/// script being replayed. A sequence ends with every node compared and
/// `check_io` run.
pub fn run(
    geometry: Geometry,
    mut next: impl FnMut(&Model, usize) -> Option<Op>,
    cfg: &Config,
    stats: &mut Stats,
) -> Result<Finished, Failure> {
    let started = Instant::now();
    let mut ops: Vec<Op> = Vec::new();
    let fail = |ops: &[Op], class: &str, what: String| Failure {
        geometry,
        ops: ops.to_vec(),
        class: class.to_string(),
        what,
    };

    let mut io = MemIo {
        bytes: vec![0; geometry.size as usize],
    };
    mkfs::format_filesystem(
        &mut io,
        geometry.size,
        geometry.cluster,
        geometry.record,
        Some("OPSEQ"),
        Some(0x0123_4567_89ab_cdef),
    )
    .map_err(|e| fail(&ops, "format", format!("format_filesystem refused: {e}")))?;
    let upcase = UpcaseTable::load_io(&mut io)
        .map_err(|e| fail(&ops, "format", format!("$UpCase does not load: {e}")))?;
    let root = read::resolve_path(&mut io, "/")
        .map_err(|e| fail(&ops, "format", format!("the root does not resolve: {e}")))?;
    let mut model = Model::new(upcase, root);
    stats.sequences += 1;

    for i in 0..cfg.ops {
        let Some(op) = next(&model, i) else { break };
        ops.push(op.clone());
        stats.ops += 1;
        let expect = model.expect(&op);
        let result = catch_unwind(AssertUnwindSafe(|| drive(&mut io, &op)));
        let result = match result {
            Ok(r) => r,
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                return Err(fail(&ops, &format!("panic:{}", op.verb()), format!("the driver panicked: {msg}")));
            }
        };
        let outcome = match (&expect, result) {
            (Expect::Refuse(why), Ok(_)) => {
                return Err(fail(
                    &ops,
                    &format!("accepted:{}", op.verb()),
                    format!("the driver accepted the call, and it had to refuse: {why}"),
                ))
            }
            (Expect::Refuse(_), Err(e)) => {
                stats.refusal(&op, true, &e);
                format!("refused as it had to: {e}")
            }
            (Expect::Either, Err(e)) => {
                stats.refusal(&op, false, &e);
                format!("refused: {e}")
            }
            (Expect::Either, Ok(v)) => {
                stats.accepted += 1;
                model
                    .apply(&op, v)
                    .map_err(|why| fail(&ops, &format!("result:{}", op.verb()), why))?;
                format!("accepted ({v})")
            }
        };

        // The nodes the call could have changed, and every other name of
        // each file among them.
        let mut paths = op.touched();
        for p in op.touched() {
            if let Some(id) = model.lookup(&p) {
                if !model.nodes[&id].is_dir() {
                    paths.extend(model.paths().into_iter().filter(|(_, n)| *n == id).map(|(p, _)| p));
                }
            }
        }
        for p in &paths {
            verify_path(&mut io, &model, p).map_err(|m| {
                fail(&ops, &m.class, format!("after the call ({outcome}): {}", m.detail))
            })?;
        }

        if (i + 1) % cfg.checkpoint_every == 0 {
            verify_all(&mut io, &model).map_err(|m| {
                fail(&ops, &m.class, format!("at the checkpoint after it ({outcome}): {}", m.detail))
            })?;
        }
        if started.elapsed() > cfg.deadline {
            return Err(fail(
                &ops,
                "deadline",
                format!(
                    "the sequence passed its {} s deadline at operation {} of {}",
                    cfg.deadline.as_secs(),
                    i + 1,
                    cfg.ops
                ),
            ));
        }
    }
    verify_all(&mut io, &model)
        .map_err(|m| fail(&ops, &m.class, format!("at the end of the sequence: {}", m.detail)))?;
    Ok(Finished { io, model })
}

/// A sequence generated from `seed`.
pub fn run_seed(seed: u64, cfg: &Config, stats: &mut Stats) -> Result<Finished, Failure> {
    let mut rng = Rng::new(seed);
    let geometry = Geometry::from_rng(&mut rng);
    let mut generator = Generator::new(rng);
    run(geometry, |model, _| Some(generator.next_op(model)), cfg, stats)
}

pub fn geometry_of_seed(seed: u64) -> Geometry {
    Geometry::from_rng(&mut Rng::new(seed))
}

/// Replay a script.
pub fn run_script(
    geometry: Geometry,
    ops: &[Op],
    cfg: &Config,
    stats: &mut Stats,
) -> Result<Finished, Failure> {
    let cfg = Config {
        ops: ops.len(),
        checkpoint_every: cfg.checkpoint_every,
        deadline: cfg.deadline,
    };
    run(geometry, |_, i| ops.get(i).cloned(), &cfg, stats)
}

/// Make a failing script shorter while it fails the same way: drop
/// chunks of operations, halving the chunk until single operations, and
/// keep each removal that still fails with the same class. At most
/// `budget` replays.
pub fn shrink(failure: Failure, cfg: &Config, budget: usize) -> Failure {
    let mut best = failure;
    let mut replays = 0;
    let mut chunk = best.ops.len().div_ceil(2).max(1);
    loop {
        let mut start = 0;
        while start < best.ops.len() && replays < budget {
            let mut candidate = best.ops.clone();
            let end = (start + chunk).min(candidate.len());
            candidate.drain(start..end);
            replays += 1;
            let mut scratch = Stats::default();
            match run_script(best.geometry, &candidate, cfg, &mut scratch) {
                Err(f) if f.class == best.class => best = f,
                _ => start += chunk,
            }
        }
        if chunk == 1 || replays >= budget {
            return best;
        }
        chunk = chunk.div_ceil(2);
    }
}
