//! Test-only operations and name-pool cases extracted from the preserved #484
//! engine. Byte decoding and strict replay are independent of NTFS disk APIs.
use super::model::{Error, Model};
use serde::{Deserialize, Serialize};

pub const MAX_STEPS: usize = 120;
pub const MAX_DATA: usize = 4096;
pub const MAX_PATH: usize = 4096;
pub const MAX_SIZE: u32 = 65536;
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
}
impl Op {
    fn bounded(&self) -> bool {
        let primary = match self {
            Self::Create { parent, .. } | Self::Mkdir { parent, .. } => parent,
            Self::Write { path, .. }
            | Self::WriteAt { path, .. }
            | Self::Truncate { path, .. }
            | Self::Rename { path, .. }
            | Self::Unlink { path }
            | Self::Rmdir { path }
            | Self::Link { path, .. } => path,
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replay {
    version: u8,
    operations: Vec<Op>,
}
fn validate(operations: &[Op]) -> Result<(), String> {
    if operations.len() > MAX_STEPS || !operations.iter().all(Op::bounded) {
        return Err("operation workload bound exceeded".into());
    }
    Ok(())
}
/// Versioned concrete operations: replay never resolves selectors again.
pub fn encode(operations: &[Op]) -> Result<Vec<u8>, String> {
    validate(operations)?;
    let bytes = serde_json::to_vec(&Replay {
        version: 1,
        operations: operations.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REPLAY {
        return Err("replay byte bound exceeded".into());
    }
    Ok(bytes)
}
pub fn decode_replay(bytes: &[u8]) -> Result<Vec<Op>, String> {
    if bytes.len() > MAX_REPLAY {
        return Err("replay byte bound exceeded".into());
    }
    let replay: Replay = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if replay.version != 1 {
        return Err("unsupported replay version".into());
    }
    validate(&replay.operations)?;
    Ok(replay.operations)
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
    /// payload length/size, LE u16 offset. Only writes consume payload bytes.
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
        let verb = h[0] % 9;
        let path = select(model, h[1], verb == 7, false);
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
        if verb == 2 || verb == 3 {
            let len = (raw as usize % (MAX_DATA + 1)).min(self.remaining.len());
            data.extend_from_slice(&self.remaining[..len]);
            self.remaining = &self.remaining[len..];
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
            _ => Op::Link { path, parent, name },
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

/// Interface only (#684): the implementation follows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regression {
    pub kind: String,
    pub step: usize,
    pub geometry: Geometry,
    pub original: String,
    pub original_operations: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub size: u64,
    pub cluster: u32,
    pub record: u32,
    pub label: String,
    pub serial: u64,
}
pub fn encode_regression(_operations: &[Op], _regression: &Regression) -> Result<Vec<u8>, String> {
    unimplemented!("#684")
}
pub fn decode_regression(_bytes: &[u8]) -> Result<(Vec<Op>, Option<Regression>), String> {
    unimplemented!("#684")
}
