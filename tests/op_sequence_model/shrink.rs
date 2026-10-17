//! Minimization of a failing operation sequence into a pinned regression
//! (#684). Each candidate is a whole replay supplied by the caller; nothing
//! here reads NTFS, draws random numbers or depends on the generator's bytes.
use super::generator::{self, Op, Regression};
use std::any::Any;
use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic;
use std::path::{Path, PathBuf};

/// Two mismatches are the same when their kinds match and each was observed
/// after the same number of executed operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub kind: String,
    pub step: usize,
}

/// A failure message's stable label: its first line, after any
/// `assertion ... failed: ` prefix, up to the first `: `, path (` /`), `{`
/// or `"`. What follows those varies as a sequence shrinks.
pub fn kind(message: &str) -> String {
    let line = message.lines().next().unwrap_or("");
    let line = match line.split_once(" failed: ") {
        Some((head, tail)) if head.starts_with("assertion") => tail,
        _ => line,
    };
    let end = [": ", " /", "{", "\""]
        .iter()
        .filter_map(|cut| line.find(cut))
        .min()
        .unwrap_or(line.len());
    match line[..end].trim() {
        "" => "panic".into(),
        label => label.into(),
    }
}

pub fn kind_of(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        kind(message)
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        kind(message)
    } else {
        "panic".into()
    }
}

thread_local!(static QUIET: Cell<bool> = const { Cell::new(false) });

/// Run a candidate whose panics are expected; see `quiet_panics`.
pub fn quietly<T>(candidate: impl FnOnce() -> T) -> T {
    QUIET.set(true);
    let result = candidate();
    QUIET.set(false);
    result
}

/// For the capture command only: a report per failing candidate would bury
/// its result. Every panic outside `quietly` is still reported.
pub fn quiet_panics() {
    let report = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if !QUIET.get() {
            report(info)
        }
    }));
}

/// Remove, then shorten, operations while the same mismatch reproduces,
/// until neither changes anything. The operation the mismatch follows stays
/// last, so a candidate reproduces only if it fails with the same kind after
/// its own final operation. Candidates are tried in one fixed order.
pub fn shrink(
    operations: &[Op],
    found: &Mismatch,
    mut reproduces: impl FnMut(&[Op]) -> Option<Mismatch>,
) -> Result<Vec<Op>, String> {
    let mut same = |candidate: &[Op]| {
        reproduces(candidate)
            == Some(Mismatch {
                kind: found.kind.clone(),
                step: candidate.len(),
            })
    };
    let mut current = operations
        .get(..found.step)
        .ok_or("the mismatch follows more operations than the input has")?
        .to_vec();
    if !same(&current) {
        return Err(format!(
            "{} after operation {} does not reproduce",
            found.kind, found.step
        ));
    }
    loop {
        let removed = remove(&mut current, &mut same);
        let shortened = shorten(&mut current, &mut same);
        if !removed && !shortened {
            return Ok(current);
        }
    }
}

/// Delete runs of operations before the pinned one, halving the run length
/// down to a single operation.
fn remove(current: &mut Vec<Op>, same: &mut impl FnMut(&[Op]) -> bool) -> bool {
    let mut changed = false;
    let mut size = current.len().saturating_sub(1).div_ceil(2);
    while size > 0 {
        let mut start = 0;
        while start + size < current.len() {
            let candidate = [&current[..start], &current[start + size..]].concat();
            if same(&candidate) {
                *current = candidate;
                changed = true;
            } else {
                start += size;
            }
        }
        size /= 2;
    }
    changed
}

/// Shorten payloads, offsets and sizes, from the last operation back.
fn shorten(current: &mut [Op], same: &mut impl FnMut(&[Op]) -> bool) -> bool {
    let mut changed = false;
    for index in (0..current.len()).rev() {
        'retry: loop {
            for simpler in simpler(&current[index]) {
                let mut candidate = current.to_vec();
                candidate[index] = simpler;
                if same(&candidate) {
                    current[index] = candidate[index].clone();
                    changed = true;
                    continue 'retry;
                }
            }
            break;
        }
    }
    changed
}

/// Strictly smaller values, smallest first: zero, one, half, one less.
fn smaller(value: usize) -> Vec<usize> {
    let mut values: Vec<_> = [0, 1, value / 2, value.saturating_sub(1)]
        .into_iter()
        .filter(|v| *v < value)
        .collect();
    values.sort();
    values.dedup();
    values
}

/// Data keeps its prefix. Paths and names are never changed: later
/// operations name them, so changing one would change what those do.
fn simpler(op: &Op) -> Vec<Op> {
    match op {
        Op::Write { path, data } => smaller(data.len())
            .into_iter()
            .map(|n| Op::Write {
                path: path.clone(),
                data: data[..n].to_vec(),
            })
            .collect(),
        Op::WriteAt { path, offset, data } => {
            let shorter = smaller(data.len()).into_iter().map(|n| Op::WriteAt {
                path: path.clone(),
                offset: *offset,
                data: data[..n].to_vec(),
            });
            let earlier = smaller(*offset as usize).into_iter().map(|o| Op::WriteAt {
                path: path.clone(),
                offset: o as u32,
                data: data.clone(),
            });
            shorter.chain(earlier).collect()
        }
        Op::Truncate { path, size } => smaller(*size as usize)
            .into_iter()
            .map(|s| Op::Truncate {
                path: path.clone(),
                size: s as u32,
            })
            .collect(),
        Op::WriteStream { path, stream, data } => smaller(data.len())
            .into_iter()
            .map(|n| Op::WriteStream {
                path: path.clone(),
                stream: stream.clone(),
                data: data[..n].to_vec(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `<corpus>/<name>-v1.json`, refused when it exists or the name does not
/// describe a finding in lowercase words joined by hyphens.
pub fn destination(corpus: &Path, name: &str) -> Result<PathBuf, String> {
    let word = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    };
    if name.len() > 64 || !name.split('-').all(word) {
        return Err(format!(
            "{name:?}: name a regression with lowercase words and hyphens"
        ));
    }
    let path = corpus.join(format!("{name}-v1.json"));
    if path.exists() {
        return Err(format!("{} exists: choose another name", path.display()));
    }
    Ok(path)
}

/// Write the minimized operations with their pin. Never overwrites.
pub fn pin(path: &Path, operations: &[Op], regression: &Regression) -> Result<PathBuf, String> {
    let mut bytes = generator::encode_regression(operations, regression)?;
    bytes.push(b'\n');
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(&bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.to_path_buf())
}
