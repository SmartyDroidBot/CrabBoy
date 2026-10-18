//! What a title saves: its save data, its extra data and the SD card, as
//! volumes of directories and files in memory (3dbrew, "Savegames",
//! "Extdata").
//!
//! The store is what `System::save_data` hands a frontend and
//! `System::load_data` takes back, so saves persist with no frontend code of
//! their own. A volume exists once it is formatted (save data) or created
//! (extra data); until then opening it fails as on a console that never
//! held one. Writes take effect at once: the commit a title asks for is not
//! needed to keep them, and a save is written out whole.

use std::collections::BTreeMap;

/// Why a file system operation failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsError {
    /// No such volume.
    NoVolume,
    /// No file or directory at the path.
    NotFound,
    /// The directory the path lies in does not exist.
    NoParent,
    /// Something is already at the path.
    Exists,
    /// A directory still holds entries.
    NotEmpty,
    /// The path leaves the volume or has no name.
    BadPath,
}

/// How a volume was formatted.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Format {
    pub size: u64,
    pub directories: u32,
    pub files: u32,
    pub duplicate: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Node {
    Directory,
    File(Vec<u8>),
}

/// A directory entry, as a listing returns it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Volume {
    pub format: Format,
    /// By normalised path; the root is implied.
    nodes: BTreeMap<String, Node>,
}

/// `path` with its separators tidied and `.` parts dropped: `/a/b`, or `/`
/// for the root. `..` is refused.
pub fn normalize(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &path[..at],
    }
}

impl Volume {
    fn is_directory(&self, path: &str) -> bool {
        path == "/" || self.nodes.get(path) == Some(&Node::Directory)
    }

    /// A new node at `path`, whose parent must be a directory.
    fn create(&mut self, path: &str, node: Node) -> Result<(), FsError> {
        if path == "/" {
            return Err(FsError::Exists);
        }
        if !self.is_directory(parent(path)) {
            return Err(FsError::NoParent);
        }
        if self.nodes.contains_key(path) {
            return Err(FsError::Exists);
        }
        self.nodes.insert(path.to_string(), node);
        Ok(())
    }

    fn file(&self, path: &str) -> Result<&Vec<u8>, FsError> {
        match self.nodes.get(path) {
            Some(Node::File(data)) => Ok(data),
            _ => Err(FsError::NotFound),
        }
    }

    fn file_mut(&mut self, path: &str) -> Result<&mut Vec<u8>, FsError> {
        match self.nodes.get_mut(path) {
            Some(Node::File(data)) => Ok(data),
            _ => Err(FsError::NotFound),
        }
    }

    /// The paths below directory `path`, at any depth.
    fn below(&self, path: &str) -> Vec<String> {
        let prefix = if path == "/" {
            "/".to_string()
        } else {
            format!("{path}/")
        };
        self.nodes
            .range(prefix.clone()..)
            .take_while(|(p, _)| p.starts_with(&prefix))
            .map(|(p, _)| p.clone())
            .collect()
    }

    pub fn used_bytes(&self) -> u64 {
        self.nodes
            .values()
            .map(|n| match n {
                Node::File(data) => data.len() as u64,
                Node::Directory => 0,
            })
            .sum()
    }
}

#[derive(Default)]
pub struct Store {
    volumes: BTreeMap<String, Volume>,
    /// Secure values, by slot (3dbrew, "Anti Savegame Restore").
    pub secure_values: BTreeMap<u32, u64>,
    dirty: bool,
}

const MAGIC: &[u8; 8] = b"CRAB3DSF";
const VERSION: u32 = 1;

impl Store {
    /// Nothing was ever saved.
    pub fn is_empty(&self) -> bool {
        self.volumes.is_empty() && self.secure_values.is_empty()
    }

    pub fn volume(&self, key: &str) -> Option<&Volume> {
        self.volumes.get(key)
    }

    fn volume_mut(&mut self, key: &str) -> Result<&mut Volume, FsError> {
        self.dirty = true;
        self.volumes.get_mut(key).ok_or(FsError::NoVolume)
    }

    /// Format `key` afresh: whatever it held is gone.
    pub fn format(&mut self, key: &str, format: Format) {
        self.dirty = true;
        self.volumes.insert(
            key.to_string(),
            Volume {
                format,
                nodes: BTreeMap::new(),
            },
        );
    }

    /// Create `key` if it does not exist yet.
    pub fn create_volume(&mut self, key: &str, format: Format) -> Result<(), FsError> {
        if self.volumes.contains_key(key) {
            return Err(FsError::Exists);
        }
        self.format(key, format);
        Ok(())
    }

    pub fn delete_volume(&mut self, key: &str) -> Result<(), FsError> {
        self.dirty = true;
        self.volumes
            .remove(key)
            .map(|_| ())
            .ok_or(FsError::NoVolume)
    }

    pub fn is_file(&self, key: &str, path: &str) -> bool {
        self.volume(key).is_some_and(|v| v.file(path).is_ok())
    }

    pub fn is_directory(&self, key: &str, path: &str) -> bool {
        self.volume(key).is_some_and(|v| v.is_directory(path))
    }

    pub fn create_file(&mut self, key: &str, path: &str, size: u64) -> Result<(), FsError> {
        let data = vec![0; usize::try_from(size).map_err(|_| FsError::BadPath)?];
        self.volume_mut(key)?.create(path, Node::File(data))
    }

    pub fn create_directory(&mut self, key: &str, path: &str) -> Result<(), FsError> {
        self.volume_mut(key)?.create(path, Node::Directory)
    }

    pub fn delete_file(&mut self, key: &str, path: &str) -> Result<(), FsError> {
        let volume = self.volume_mut(key)?;
        volume.file(path)?;
        volume.nodes.remove(path);
        Ok(())
    }

    /// Remove directory `path`, which must be empty unless `recursive`.
    pub fn delete_directory(
        &mut self,
        key: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), FsError> {
        let volume = self.volume_mut(key)?;
        if path == "/" || !volume.is_directory(path) {
            return Err(FsError::NotFound);
        }
        let below = volume.below(path);
        if !below.is_empty() && !recursive {
            return Err(FsError::NotEmpty);
        }
        for p in below {
            volume.nodes.remove(&p);
        }
        volume.nodes.remove(path);
        Ok(())
    }

    /// Move file or directory `from` to `to`, with what lies below it.
    pub fn rename(
        &mut self,
        key: &str,
        from: &str,
        to: &str,
        directory: bool,
    ) -> Result<(), FsError> {
        let volume = self.volume_mut(key)?;
        let found = match volume.nodes.get(from) {
            Some(Node::Directory) => directory,
            Some(Node::File(_)) => !directory,
            None => false,
        };
        if !found {
            return Err(FsError::NotFound);
        }
        if volume.nodes.contains_key(to) || to == "/" {
            return Err(FsError::Exists);
        }
        if !volume.is_directory(parent(to)) || to.starts_with(&format!("{from}/")) {
            return Err(FsError::NoParent);
        }
        let moved: Vec<String> = std::iter::once(from.to_string())
            .chain(volume.below(from))
            .collect();
        for p in moved {
            let node = volume.nodes.remove(&p).expect("listed above");
            volume
                .nodes
                .insert(format!("{to}{}", &p[from.len()..]), node);
        }
        Ok(())
    }

    /// Up to `len` bytes of file `path` from `offset`.
    pub fn read(&self, key: &str, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let data = self.volume(key).ok_or(FsError::NoVolume)?.file(path)?;
        let start = (offset.min(data.len() as u64)) as usize;
        let end = start.saturating_add(len).min(data.len());
        Ok(data[start..end].to_vec())
    }

    /// Write `bytes` at `offset`, growing the file as needed (with zeros
    /// between its end and `offset`).
    pub fn write(
        &mut self,
        key: &str,
        path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), FsError> {
        let data = self.volume_mut(key)?.file_mut(path)?;
        let start = usize::try_from(offset).map_err(|_| FsError::BadPath)?;
        let end = start + bytes.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(bytes);
        Ok(())
    }

    pub fn size(&self, key: &str, path: &str) -> Result<u64, FsError> {
        Ok(self.volume(key).ok_or(FsError::NoVolume)?.file(path)?.len() as u64)
    }

    pub fn set_size(&mut self, key: &str, path: &str, size: u64) -> Result<(), FsError> {
        let size = usize::try_from(size).map_err(|_| FsError::BadPath)?;
        self.volume_mut(key)?.file_mut(path)?.resize(size, 0);
        Ok(())
    }

    /// The entries directly in directory `path`, in name order.
    pub fn list(&self, key: &str, path: &str) -> Result<Vec<Entry>, FsError> {
        let volume = self.volume(key).ok_or(FsError::NoVolume)?;
        if !volume.is_directory(path) {
            return Err(FsError::NotFound);
        }
        Ok(volume
            .below(path)
            .into_iter()
            .filter(|p| parent(p) == path)
            .map(|p| {
                let node = &volume.nodes[&p];
                Entry {
                    name: p[p.rfind('/').unwrap_or(0) + 1..].to_string(),
                    directory: *node == Node::Directory,
                    size: match node {
                        Node::File(data) => data.len() as u64,
                        Node::Directory => 0,
                    },
                }
            })
            .collect())
    }

    pub fn set_secure_value(&mut self, slot: u32, value: u64) {
        self.dirty = true;
        self.secure_values.insert(slot, value);
    }

    /// True once after anything changed.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The whole store, little-endian: volumes with their format and nodes,
    /// then the secure values.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        let string = |out: &mut Vec<u8>, s: &str| {
            out.extend((s.len() as u32).to_le_bytes());
            out.extend(s.as_bytes());
        };
        out.extend(VERSION.to_le_bytes());
        out.extend((self.volumes.len() as u32).to_le_bytes());
        for (key, volume) in &self.volumes {
            string(&mut out, key);
            let f = volume.format;
            out.extend(f.size.to_le_bytes());
            out.extend(f.directories.to_le_bytes());
            out.extend(f.files.to_le_bytes());
            out.push(f.duplicate as u8);
            out.extend((volume.nodes.len() as u32).to_le_bytes());
            for (path, node) in &volume.nodes {
                string(&mut out, path);
                match node {
                    Node::Directory => out.push(0),
                    Node::File(data) => {
                        out.push(1);
                        out.extend((data.len() as u64).to_le_bytes());
                        out.extend(data);
                    }
                }
            }
        }
        out.extend((self.secure_values.len() as u32).to_le_bytes());
        for (slot, value) in &self.secure_values {
            out.extend(slot.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out
    }

    /// A store from `encode`'s bytes; `None` if they are not one.
    pub fn decode(bytes: &[u8]) -> Option<Store> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(8)? != MAGIC || r.u32()? != VERSION {
            return None;
        }
        let mut store = Store::default();
        for _ in 0..r.u32()? {
            let key = r.string()?;
            let format = Format {
                size: r.u64()?,
                directories: r.u32()?,
                files: r.u32()?,
                duplicate: r.take(1)?[0] != 0,
            };
            let mut nodes = BTreeMap::new();
            for _ in 0..r.u32()? {
                let path = r.string()?;
                let node = match r.take(1)?[0] {
                    0 => Node::Directory,
                    _ => {
                        let len = usize::try_from(r.u64()?).ok()?;
                        Node::File(r.take(len)?.to_vec())
                    }
                };
                nodes.insert(path, node);
            }
            store.volumes.insert(key, Volume { format, nodes });
        }
        for _ in 0..r.u32()? {
            let slot = r.u32()?;
            store.secure_values.insert(slot, r.u64()?);
        }
        Some(store)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.at..self.at.checked_add(len)?)?;
        self.at += len;
        Some(slice)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_normalise_and_cannot_climb_out() {
        assert_eq!(normalize("/a//b/./c").as_deref(), Some("/a/b/c"));
        assert_eq!(normalize("").as_deref(), Some("/"));
        assert_eq!(normalize("/a/../b"), None);
        assert_eq!(parent("/a/b"), "/a");
        assert_eq!(parent("/a"), "/");
    }

    #[test]
    fn files_live_in_directories_that_exist() {
        let mut s = Store::default();
        assert_eq!(s.create_file("v", "/a", 0), Err(FsError::NoVolume));
        s.format("v", Format::default());
        assert_eq!(s.create_file("v", "/d/a", 0), Err(FsError::NoParent));
        s.create_directory("v", "/d").unwrap();
        s.create_file("v", "/d/a", 4).unwrap();
        assert_eq!(s.create_file("v", "/d/a", 0), Err(FsError::Exists));
        s.write("v", "/d/a", 6, b"xy").unwrap();
        assert_eq!(
            s.read("v", "/d/a", 0, 100).unwrap(),
            [0, 0, 0, 0, 0, 0, b'x', b'y']
        );
        assert_eq!(s.delete_directory("v", "/d", false), Err(FsError::NotEmpty));
        s.rename("v", "/d", "/e", true).unwrap();
        assert!(s.is_file("v", "/e/a") && !s.is_directory("v", "/d"));
        let names: Vec<_> = s
            .list("v", "/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["e"]);
        s.delete_directory("v", "/e", true).unwrap();
        assert!(s.list("v", "/").unwrap().is_empty());
    }

    #[test]
    fn the_store_round_trips_through_its_encoding() {
        let mut s = Store::default();
        s.format(
            "save",
            Format {
                size: 0x10_0000,
                directories: 10,
                files: 10,
                duplicate: true,
            },
        );
        s.create_directory("save", "/x").unwrap();
        s.create_file("save", "/x/main", 3).unwrap();
        s.write("save", "/x/main", 0, b"abc").unwrap();
        s.set_secure_value(0x1000, 42);
        assert!(s.take_dirty() && !s.take_dirty());
        let back = Store::decode(&s.encode()).unwrap();
        assert_eq!(back.volumes, s.volumes);
        assert_eq!(back.secure_values, s.secure_values);
        assert!(Store::decode(b"nonsense").is_none());
    }
}
