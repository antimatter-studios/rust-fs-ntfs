//! Expected logical state only: no NTFS readers, writers, record IDs or layouts.
//! The node/entry design comes from the preserved op-sequence engine (#484).
use std::collections::BTreeMap;

/// A workload bound, not a claim about NTFS's maximum file size.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
type Directory = BTreeMap<Vec<u16>, (String, usize)>;
/// Named `$DATA` streams by `$UpCase` key: stored spelling and bytes.
type Streams = BTreeMap<Vec<u16>, (String, Vec<u8>)>;
/// `FILE_NEED_EA`, the one EA flag Windows defines.
pub const NEED_EA: u8 = 0x80;
/// One EA as a file holds it: stored name, flags and value (#732).
pub type Ea = (Vec<u8>, u8, Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidTable,
    InvalidName,
    InvalidPath,
    Missing,
    NotDirectory,
    IsDirectory,
    Exists,
    NotEmpty,
    FileTooLarge,
    /// A file's EAs would pack to more than `$EA_INFORMATION` can count.
    EaSetTooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    File(Vec<u8>),
    Directory(Directory),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    kind: Kind,
    links: usize,
    /// A node's streams, not a name's: every hard link shares them (#731).
    streams: Streams,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    /// Model identity, independent of any driver-assigned record number.
    pub id: usize,
    pub directory: bool,
    pub size: usize,
    pub links: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    nodes: BTreeMap<usize, Node>,
    next: usize,
    upcase: Vec<u16>,
}

impl Model {
    /// Accept the mounted volume's entire UTF-16 mapping as data. Folding is
    /// one table lookup per code unit; Unicode expansion is never involved.
    pub fn new(upcase: Vec<u16>) -> Result<Self, Error> {
        if upcase.len() != 65536 {
            return Err(Error::InvalidTable);
        }
        let root = Node {
            kind: Kind::Directory(BTreeMap::new()),
            links: 1,
            streams: Streams::new(),
        };
        Ok(Self {
            nodes: BTreeMap::from([(0, root)]),
            next: 1,
            upcase,
        })
    }

    fn key(&self, name: &str) -> Vec<u16> {
        name.encode_utf16()
            .map(|c| self.upcase[c as usize])
            .collect()
    }

    fn valid_name(name: &str) -> bool {
        !name.is_empty()
            && name != "."
            && name != ".."
            && name.encode_utf16().count() <= 255
            && !name.chars().any(|c| "\0/\\:*?\"<>|".contains(c))
    }

    fn name_key(&self, name: &str) -> Result<Vec<u16>, Error> {
        if !Self::valid_name(name) {
            return Err(Error::InvalidName);
        }
        Ok(self.key(name))
    }

    fn dir(&self, id: usize) -> Result<&Directory, Error> {
        match &self.nodes[&id].kind {
            Kind::Directory(entries) => Ok(entries),
            Kind::File(_) => Err(Error::NotDirectory),
        }
    }

    fn dir_mut(&mut self, id: usize) -> &mut Directory {
        match &mut self.nodes.get_mut(&id).unwrap().kind {
            Kind::Directory(entries) => entries,
            Kind::File(_) => unreachable!("directory checked before mutation"),
        }
    }

    fn lookup(&self, path: &str) -> Result<usize, Error> {
        if path == "/" {
            return Ok(0);
        }
        if !path.starts_with('/') || path[1..].split('/').any(|c| !Self::valid_name(c)) {
            return Err(Error::InvalidPath);
        }
        let mut id = 0;
        for component in path[1..].split('/') {
            id = self
                .dir(id)?
                .get(&self.key(component))
                .ok_or(Error::Missing)?
                .1;
        }
        Ok(id)
    }

    fn entry(&self, path: &str) -> Result<(usize, Vec<u16>, usize), Error> {
        let id = self.lookup(path)?;
        if id == 0 {
            return Err(Error::InvalidPath);
        }
        let (parent, name) = path.rsplit_once('/').unwrap();
        Ok((
            self.lookup(if parent.is_empty() { "/" } else { parent })?,
            self.key(name),
            id,
        ))
    }

    pub fn stat(&self, path: &str) -> Result<Stat, Error> {
        let id = self.lookup(path)?;
        let node = &self.nodes[&id];
        let (directory, size) = match &node.kind {
            Kind::File(data) => (false, data.len()),
            Kind::Directory(_) => (true, 0),
        };
        Ok(Stat {
            id,
            directory,
            size,
            links: node.links,
        })
    }

    /// Names retain their stored spelling; ordering uses the injected table.
    pub fn list(&self, path: &str) -> Result<Vec<(String, usize)>, Error> {
        Ok(self.dir(self.lookup(path)?)?.values().cloned().collect())
    }

    pub fn read(&self, path: &str) -> Result<&[u8], Error> {
        match &self.nodes[&self.lookup(path)?].kind {
            Kind::File(data) => Ok(data),
            Kind::Directory(_) => Err(Error::IsDirectory),
        }
    }

    fn file_mut(&mut self, path: &str) -> Result<&mut Vec<u8>, Error> {
        let id = self.lookup(path)?;
        match &mut self.nodes.get_mut(&id).unwrap().kind {
            Kind::File(data) => Ok(data),
            Kind::Directory(_) => Err(Error::IsDirectory),
        }
    }

    pub fn create(&mut self, parent: &str, name: &str, directory: bool) -> Result<usize, Error> {
        let key = self.name_key(name)?;
        let parent = self.lookup(parent)?;
        if self.dir(parent)?.contains_key(&key) {
            return Err(Error::Exists);
        }
        let id = self.next;
        let kind = if directory {
            Kind::Directory(BTreeMap::new())
        } else {
            Kind::File(Vec::new())
        };
        self.nodes.insert(
            id,
            Node {
                kind,
                links: 1,
                streams: Streams::new(),
            },
        );
        self.next += 1;
        self.dir_mut(parent).insert(key, (name.into(), id));
        Ok(id)
    }

    pub fn write(&mut self, path: &str, offset: usize, bytes: &[u8]) -> Result<(), Error> {
        let data = self.file_mut(path)?;
        // An empty write never extends a file, even beyond EOF.
        if bytes.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(bytes.len())
            .filter(|end| *end <= MAX_FILE_BYTES)
            .ok_or(Error::FileTooLarge)?;
        data.resize(data.len().max(end), 0);
        data[offset..end].copy_from_slice(bytes);
        Ok(())
    }

    pub fn truncate(&mut self, path: &str, size: usize) -> Result<(), Error> {
        if size > MAX_FILE_BYTES {
            return Err(Error::FileTooLarge);
        }
        self.file_mut(path)?.resize(size, 0);
        Ok(())
    }

    pub fn link(&mut self, path: &str, parent: &str, name: &str) -> Result<(), Error> {
        let id = self.lookup(path)?;
        if matches!(self.nodes[&id].kind, Kind::Directory(_)) {
            return Err(Error::IsDirectory);
        }
        let parent = self.lookup(parent)?;
        let key = self.name_key(name)?;
        if self.dir(parent)?.contains_key(&key) {
            return Err(Error::Exists);
        }
        self.dir_mut(parent).insert(key, (name.into(), id));
        self.nodes.get_mut(&id).unwrap().links += 1;
        Ok(())
    }

    fn drop_link(&mut self, id: usize) {
        let node = self.nodes.get_mut(&id).unwrap();
        node.links -= 1;
        if node.links == 0 {
            self.nodes.remove(&id);
        }
    }

    /// Remove one file name or an empty directory. Open handles are outside
    /// this path-based model; the last unlink discards the file's bytes.
    pub fn remove(&mut self, path: &str) -> Result<(), Error> {
        let (parent, key, id) = self.entry(path)?;
        if matches!(&self.nodes[&id].kind, Kind::Directory(e) if !e.is_empty()) {
            return Err(Error::NotEmpty);
        }
        self.dir_mut(parent).remove(&key);
        self.drop_link(id);
        Ok(())
    }

    /// A name Windows can open as a stream: at most 255 UTF-16 units, no
    /// reserved or control character, and no trailing space or period,
    /// which Win32 path parsing strips.
    fn valid_stream_name(name: &str) -> bool {
        Self::valid_name(name)
            && !name.chars().any(|c| (c as u32) < 0x20)
            && !name.ends_with(' ')
            && !name.ends_with('.')
    }

    /// Create or replace the named `$DATA` stream `name` of `path` (#731).
    /// A name equal under `$UpCase` to an existing one names that stream,
    /// and its first spelling is kept. A directory holds streams too.
    pub fn write_stream(&mut self, path: &str, name: &str, bytes: &[u8]) -> Result<(), Error> {
        if !Self::valid_stream_name(name) {
            return Err(Error::InvalidName);
        }
        let id = self.lookup(path)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(Error::FileTooLarge);
        }
        let key = self.key(name);
        let streams = &mut self.nodes.get_mut(&id).unwrap().streams;
        let spelling = streams.get(&key).map_or(name.into(), |(n, _)| n.clone());
        streams.insert(key, (spelling, bytes.to_vec()));
        Ok(())
    }

    /// Delete the named `$DATA` stream `name` of `path` (#731).
    pub fn delete_stream(&mut self, path: &str, name: &str) -> Result<(), Error> {
        if name.is_empty() {
            return Err(Error::InvalidName);
        }
        let id = self.lookup(path)?;
        let key = self.key(name);
        let streams = &mut self.nodes.get_mut(&id).unwrap().streams;
        streams.remove(&key).map(|_| ()).ok_or(Error::Missing)
    }

    /// The named streams of `path`, in `$UpCase` key order (#731).
    pub fn streams(&self, path: &str) -> Result<Vec<(String, Vec<u8>)>, Error> {
        Ok(self.nodes[&self.lookup(path)?]
            .streams
            .values()
            .cloned()
            .collect())
    }

    /// Create or replace the EA `name` of `path` (#732).
    pub fn write_ea(
        &mut self,
        _path: &str,
        _name: &[u8],
        _flags: u8,
        _value: &[u8],
    ) -> Result<(), Error> {
        let _refused_when_too_large = Error::EaSetTooLarge;
        unimplemented!("extended attributes are modeled in #732")
    }

    /// Remove the EA `name` of `path` (#732).
    pub fn remove_ea(&mut self, _path: &str, _name: &[u8]) -> Result<(), Error> {
        unimplemented!("extended attributes are modeled in #732")
    }

    /// The EAs of `path`, in stored-name order (#732).
    pub fn eas(&self, _path: &str) -> Result<Vec<Ea>, Error> {
        unimplemented!("extended attributes are modeled in #732")
    }

    /// Rename within a directory, matching the initial workload's API.
    /// Preconditions are checked before any namespace or link count changes.
    pub fn rename(&mut self, path: &str, name: &str, replace: bool) -> Result<(), Error> {
        let (parent, old, id) = self.entry(path)?;
        let key = self.name_key(name)?;
        if key != old {
            if let Some((_, dest)) = self.dir(parent)?.get(&key) {
                let dest = *dest;
                if !replace {
                    return Err(Error::Exists);
                }
                // Two different names of one inode: replacement is a no-op.
                if dest == id {
                    return Ok(());
                }
                match (&self.nodes[&id].kind, &self.nodes[&dest].kind) {
                    (Kind::File(_), Kind::Directory(_)) => return Err(Error::IsDirectory),
                    (Kind::Directory(_), Kind::File(_)) => return Err(Error::NotDirectory),
                    (_, Kind::Directory(e)) if !e.is_empty() => return Err(Error::NotEmpty),
                    _ => {}
                }
                self.drop_link(dest);
            }
        }
        self.dir_mut(parent).remove(&old);
        self.dir_mut(parent).insert(key, (name.into(), id));
        Ok(())
    }
}
