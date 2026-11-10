//! Test-only operations and name-pool cases extracted from the preserved #484
//! engine. Byte decoding and strict replay are independent of NTFS disk APIs.
use super::model::{Error, Model, NEED_EA};
use serde::{Deserialize, Serialize};

pub const MAX_STEPS: usize = 120;
pub const MAX_DATA: usize = 4096;
pub const MAX_PATH: usize = 4096;
pub const MAX_SIZE: u32 = 65536;
/// The longest EA value a replay holds: the width of `EaValueLength`.
pub const MAX_EA_VALUE: usize = u16::MAX as usize;
/// Operation kinds, in opcode order: histograms have one slot for each.
pub const OPCODES: usize = 13;
const MAX_REPLAY: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Create {
        parent: String,
        name: String,
    },
    Mkdir {
        parent: String,
        name: String,
    },
    Write {
        path: String,
        data: Vec<u8>,
    },
    WriteAt {
        path: String,
        offset: u32,
        data: Vec<u8>,
    },
    Truncate {
        path: String,
        size: u32,
    },
    Rename {
        path: String,
        name: String,
        replace: bool,
    },
    Unlink {
        path: String,
    },
    Rmdir {
        path: String,
    },
    Link {
        path: String,
        parent: String,
        name: String,
    },
    /// Create or replace the named `$DATA` stream `stream` (#731).
    WriteStream {
        path: String,
        stream: String,
        data: Vec<u8>,
    },
    DeleteStream {
        path: String,
        stream: String,
    },
    /// Create or replace the extended attribute `name` (#732).
    WriteEa {
        path: String,
        name: String,
        flags: u8,
        value: Vec<u8>,
    },
    RemoveEa {
        path: String,
        name: String,
    },
}
impl Op {
    /// The opcode that generates this kind, and its histogram slot.
    pub fn opcode(&self) -> usize {
        match self {
            Self::Create { .. } => 0,
            Self::Mkdir { .. } => 1,
            Self::Write { .. } => 2,
            Self::WriteAt { .. } => 3,
            Self::Truncate { .. } => 4,
            Self::Rename { .. } => 5,
            Self::Unlink { .. } => 6,
            Self::Rmdir { .. } => 7,
            Self::Link { .. } => 8,
            Self::WriteStream { .. } => 9,
            Self::DeleteStream { .. } => 10,
            Self::WriteEa { .. } => 11,
            Self::RemoveEa { .. } => 12,
        }
    }
    fn bounded(&self) -> bool {
        let primary = match self {
            Self::Create { parent, .. } | Self::Mkdir { parent, .. } => parent,
            Self::Write { path, .. }
            | Self::WriteAt { path, .. }
            | Self::Truncate { path, .. }
            | Self::Rename { path, .. }
            | Self::Unlink { path }
            | Self::Rmdir { path }
            | Self::Link { path, .. }
            | Self::WriteStream { path, .. }
            | Self::DeleteStream { path, .. }
            | Self::WriteEa { path, .. }
            | Self::RemoveEa { path, .. } => path,
        };
        primary.len() <= MAX_PATH
            && match self {
                Self::Create { name, .. }
                | Self::Mkdir { name, .. }
                | Self::Rename { name, .. } => name.encode_utf16().count() <= 256,
                Self::Link { parent, name, .. } => {
                    parent.len() <= MAX_PATH && name.encode_utf16().count() <= 256
                }
                Self::Write { data, .. } => data.len() <= MAX_DATA,
                Self::WriteAt { offset, data, .. } => {
                    data.len() <= MAX_DATA
                        && u64::from(*offset) + data.len() as u64 <= u64::from(MAX_SIZE)
                }
                Self::Truncate { size, .. } => *size <= MAX_SIZE,
                Self::WriteStream { stream, data, .. } => {
                    stream.encode_utf16().count() <= 256 && data.len() <= MAX_DATA
                }
                Self::DeleteStream { stream, .. } => stream.encode_utf16().count() <= 256,
                Self::WriteEa { name, value, .. } => {
                    name.len() <= 256 && value.len() <= MAX_EA_VALUE
                }
                Self::RemoveEa { name, .. } => name.len() <= 256,
                _ => true,
            }
    }
    /// Expected transition only. The harness must retain the operation even
    /// when it refuses, and apply accepted disk operations to its live model.
    pub fn apply(&self, model: &mut Model) -> Result<(), Error> {
        if !self.bounded() {
            return Err(Error::FileTooLarge);
        }
        match self {
            Self::Create { parent, name } | Self::Mkdir { parent, name } => model
                .create(parent, name, matches!(self, Self::Mkdir { .. }))
                .map(|_| ()),
            Self::Write { path, data } => {
                model.truncate(path, data.len())?;
                model.write(path, 0, data)
            }
            Self::WriteAt { path, offset, data } => model.write(path, *offset as usize, data),
            Self::Truncate { path, size } => model.truncate(path, *size as usize),
            Self::Rename {
                path,
                name,
                replace,
            } => model.rename(path, name, *replace),
            Self::Link { path, parent, name } => model.link(path, parent, name),
            Self::WriteStream { path, stream, data } => model.write_stream(path, stream, data),
            Self::DeleteStream { path, stream } => model.delete_stream(path, stream),
            Self::WriteEa {
                path,
                name,
                flags,
                value,
            } => model.write_ea(path, name.as_bytes(), *flags, value),
            Self::RemoveEa { path, name } => model.remove_ea(path, name.as_bytes()),
            Self::Unlink { path } | Self::Rmdir { path } => {
                let directory = model.stat(path)?.directory;
                if directory && matches!(self, Self::Unlink { .. }) {
                    return Err(Error::IsDirectory);
                }
                if !directory && matches!(self, Self::Rmdir { .. }) {
                    return Err(Error::NotDirectory);
                }
                model.remove(path)
            }
        }
    }
}

/// What a captured regression pins (#684). Replay never consults it: it
/// records the mismatch the operations were minimized to reproduce, the
/// image they ran on and where the unminimized input came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Regression {
    /// The harness failure's stable label, such as `size of`.
    pub kind: String,
    /// Operations executed when it was observed; the last is the pinned one.
    pub step: usize,
    pub geometry: Geometry,
    pub original: String,
    pub original_operations: usize,
}
/// The `format_filesystem` arguments of the image every replay starts on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub size: u64,
    pub cluster: u32,
    pub record: u32,
    pub label: String,
    pub serial: u64,
}

/// Input bytes that choose a sequence's image, ahead of its operations.
pub const GEOMETRY_HEADER: usize = 3;
/// The image raw input bytes run on (#1024), and the bytes left for
/// operations. Zero-padded header: volume size in MiB above 64 (to 256),
/// then the cluster and MFT record sizes, each from the powers of two
/// `format_filesystem` accepts. A zero header is the default image. A
/// cluster byte of 255 asks for 128 KiB, which the formatter refuses: the
/// model expects that refusal and the explorer counts it (#1025).
pub fn decode_geometry(bytes: &[u8]) -> (Geometry, &[u8]) {
    let mut h = [0; GEOMETRY_HEADER];
    let n = bytes.len().min(h.len());
    h[..n].copy_from_slice(&bytes[..n]);
    let geometry = Geometry {
        size: (64 + u64::from(h[0]) % 193) * 1024 * 1024,
        cluster: match h[1] {
            255 => 128 * 1024,
            b => 512 << ((u32::from(b) + 3) % 8),
        },
        record: 512 << ((u32::from(h[2]) + 1) % 6),
        label: "OPSEQ".into(),
        serial: 682,
    };
    (geometry, &bytes[n..])
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replay {
    version: u8,
    operations: Vec<Op>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    regression: Option<Regression>,
}
fn validate(operations: &[Op], regression: Option<&Regression>) -> Result<(), String> {
    if operations.len() > MAX_STEPS || !operations.iter().all(Op::bounded) {
        return Err("operation workload bound exceeded".into());
    }
    if regression.is_some_and(|r| r.kind.is_empty() || r.step > operations.len()) {
        return Err("regression pin outside its operations".into());
    }
    Ok(())
}
/// Versioned concrete operations: replay never resolves selectors again.
pub fn encode(operations: &[Op]) -> Result<Vec<u8>, String> {
    encode_replay(operations, None)
}
pub fn encode_regression(operations: &[Op], regression: &Regression) -> Result<Vec<u8>, String> {
    encode_replay(operations, Some(regression))
}
fn encode_replay(operations: &[Op], regression: Option<&Regression>) -> Result<Vec<u8>, String> {
    validate(operations, regression)?;
    let bytes = serde_json::to_vec(&Replay {
        version: 1,
        operations: operations.to_vec(),
        regression: regression.cloned(),
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REPLAY {
        return Err("replay byte bound exceeded".into());
    }
    Ok(bytes)
}
pub fn decode_replay(bytes: &[u8]) -> Result<Vec<Op>, String> {
    decode_regression(bytes).map(|(operations, _)| operations)
}
pub fn decode_regression(bytes: &[u8]) -> Result<(Vec<Op>, Option<Regression>), String> {
    if bytes.len() > MAX_REPLAY {
        return Err("replay byte bound exceeded".into());
    }
    let replay: Replay = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if replay.version != 1 {
        return Err("unsupported replay version".into());
    }
    validate(&replay.operations, replay.regression.as_ref())?;
    Ok((replay.operations, replay.regression))
}

fn join(parent: &str, name: &str) -> String {
    format!("{}/{name}", parent.trim_end_matches('/'))
}
fn paths(model: &Model, parent: &str, depth: usize, out: &mut Vec<(String, bool)>) {
    if depth > 4 || out.len() >= MAX_STEPS {
        return;
    }
    let Ok(stat) = model.stat(parent) else {
        return;
    };
    out.push((parent.into(), stat.directory));
    if stat.directory {
        for (name, _) in model.list(parent).unwrap_or_default() {
            paths(model, &join(parent, &name), depth + 1, out);
        }
    }
}
fn select(model: &Model, selector: u8, directory: bool, parent: bool) -> String {
    let mut entries = Vec::new();
    paths(model, "/", 0, &mut entries);
    entries.sort(); // Explicit UTF-8 byte ordering; no host or hash-map order.
    let want = if selector % 4 == 1 {
        !directory
    } else {
        directory
    };
    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|(p, d)| *d == want && (!parent || p.matches('/').count() < 4))
        .collect();
    if !selector.is_multiple_of(4) && !candidates.is_empty() {
        return candidates[(selector as usize / 4) % candidates.len()]
            .0
            .clone();
    }
    if model.stat("/missing").is_err() {
        "/missing".into()
    } else {
        "/.".into() // Guaranteed invalid even with a caller-supplied tree.
    }
}
fn name(model: &Model, parent: &str, selector: u8, step: usize) -> String {
    const NAMES: &[&str] = &[
        "a", "A", "b", "B", "é", "É", "σ", "Σ", "中", "😀", "", ".", "..", "a:b", "a/b",
    ];
    match selector % 21 {
        n @ 0..=14 => NAMES[n as usize].into(),
        15 => "n".repeat(255),
        16 => "😀".repeat(127),
        17 => "😀".repeat(127) + "x",
        18 => "😀".repeat(128),
        19 => model
            .list(parent)
            .ok()
            .and_then(|e| e.into_iter().next())
            .map(|(n, _)| n)
            .unwrap_or_else(|| "a".into()),
        _ => format!("fresh{step}"),
    }
}

/// Streams stay on files in this slice (#731): a directory's are modeled,
/// but rmdir does not yet give back a stream's clusters unless the
/// directory has index blocks (#735). Wrong type is a file used as a
/// directory, which no stream operation can open.
fn stream_path(model: &Model, selector: u8) -> String {
    match selector % 4 {
        0 => select(model, 0, false, false),
        1 => join(&select(model, selector | 2, false, false), "x"),
        _ => select(model, selector, false, false),
    }
}
/// The file-name pool's cases, for stream names (#731).
fn stream_name(model: &Model, path: &str, selector: u8, step: usize) -> String {
    const NAMES: &[&str] = &[
        "s", "S", "σ", "Σ", "é", "É", "中", "😀", "", ".", "..", "a:b", "a/b", "x.", "x ", "\u{1}",
    ];
    match selector % 21 {
        n @ 0..=15 => NAMES[n as usize].into(),
        16 => "n".repeat(255),
        17 => "😀".repeat(127) + "x",
        18 => "😀".repeat(128),
        19 => model
            .streams(path)
            .ok()
            .and_then(|s| s.into_iter().next())
            .map(|(n, _)| n)
            .unwrap_or_else(|| "s".into()),
        _ => format!("stream{step}"),
    }
}

/// EA names (#732): byte strings, here ASCII, folded by ASCII case only.
/// Name 12 is the target's first EA in lower case, so it matches the stored
/// upper-case name only case-insensitively.
fn ea_name(model: &Model, path: &str, selector: u8, step: usize) -> String {
    const NAMES: &[&str] = &["a", "A", "user.x", "USER.X", "User.Y", "z", "ab", "AB", ""];
    match selector % 14 {
        n @ 0..=8 => NAMES[n as usize].into(),
        9 => "n".repeat(254),
        10 => "N".repeat(254),
        11 => "n".repeat(255),
        12 => model
            .eas(path)
            .ok()
            .and_then(|e| e.into_iter().next())
            .map(|(n, _, _)| String::from_utf8_lossy(&n).to_ascii_lowercase())
            .unwrap_or_else(|| "a".into()),
        _ => format!("ea{step}"),
    }
}

pub struct Generator<'a> {
    remaining: &'a [u8],
    steps: usize,
}
impl<'a> Generator<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            remaining: bytes,
            steps: 0,
        }
    }
    /// Eight-byte, zero-padded header: verb, target, parent, name, LE u16
    /// payload length/size, LE u16 offset. Only writes, stream writes and
    /// EA writes consume payload bytes.
    /// Every call consumes input; a partial header emits one final operation.
    pub fn next(&mut self, model: &Model) -> Option<Op> {
        if self.remaining.is_empty() || self.steps == MAX_STEPS {
            return None;
        }
        let mut h = [0; 8];
        let n = self.remaining.len().min(h.len());
        h[..n].copy_from_slice(&self.remaining[..n]);
        self.remaining = &self.remaining[n..];
        self.steps += 1;
        let verb = h[0] % 13;
        let path = if verb >= 9 {
            stream_path(model, h[1])
        } else {
            select(model, h[1], verb == 7, false)
        };
        let parent = select(model, h[2], true, true);
        let rename_parent = path
            .rsplit_once('/')
            .map(|(p, _)| if p.is_empty() { "/" } else { p })
            .unwrap_or("/");
        let name = name(
            model,
            if verb == 5 { rename_parent } else { &parent },
            h[3],
            self.steps,
        );
        let raw = u16::from_le_bytes([h[4], h[5]]);
        let mut data = Vec::new();
        if matches!(verb, 2 | 3 | 9) {
            let len = (raw as usize % (MAX_DATA + 1)).min(self.remaining.len());
            data.extend_from_slice(&self.remaining[..len]);
            self.remaining = &self.remaining[len..];
        }
        if verb == 11 {
            // An EA value reaches past 64 KiB from a bounded payload: the
            // offset's low byte picks a size class for the length, payload
            // short of that size repeats, and none fills a pattern (#732).
            let size = usize::from(match h[6] % 4 {
                0 => raw % 128,
                1 => raw % 2048,
                2 => raw % 16384,
                _ => raw,
            });
            let len = size.min(MAX_DATA).min(self.remaining.len());
            let payload = &self.remaining[..len];
            self.remaining = &self.remaining[len..];
            data = if payload.is_empty() {
                (0..size).map(|i| (i % 251) as u8 + 1).collect()
            } else {
                payload.iter().copied().cycle().take(size).collect()
            };
        }
        let offset =
            u32::from(u16::from_le_bytes([h[6], h[7]])) % (MAX_SIZE - data.len() as u32 + 1);
        Some(match verb {
            0 => Op::Create { parent, name },
            1 => Op::Mkdir { parent, name },
            2 => Op::Write { path, data },
            3 => Op::WriteAt { path, offset, data },
            4 => Op::Truncate {
                path,
                size: if raw == u16::MAX {
                    MAX_SIZE
                } else {
                    u32::from(raw)
                },
            },
            5 => Op::Rename {
                path,
                name,
                replace: h[0] & 128 != 0,
            },
            6 => Op::Unlink { path },
            7 => Op::Rmdir { path },
            8 => Op::Link { path, parent, name },
            9 => Op::WriteStream {
                stream: stream_name(model, &path, h[3], self.steps),
                path,
                data,
            },
            10 => Op::DeleteStream {
                stream: stream_name(model, &path, h[3], self.steps),
                path,
            },
            11 => Op::WriteEa {
                name: ea_name(model, &path, h[3], self.steps),
                flags: if h[2] & 1 != 0 { NEED_EA } else { 0 },
                path,
                value: data,
            },
            _ => Op::RemoveEa {
                name: ea_name(model, &path, h[3], self.steps),
                path,
            },
        })
    }
}
/// Fixture-free convenience decoder. The live harness can instead call
/// `Generator::next` with its model after each disk result (including errors).
pub fn decode(bytes: &[u8], initial: &Model) -> Vec<Op> {
    let mut model = initial.clone();
    let mut generator = Generator::new(bytes);
    let mut operations = Vec::new();
    while let Some(op) = generator.next(&model) {
        let _ = op.apply(&mut model); // Refusals remain in the sequence.
        operations.push(op);
    }
    operations
}
