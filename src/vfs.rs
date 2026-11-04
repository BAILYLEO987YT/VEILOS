use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;

// -----| vfs types |-----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    FileNotFound,
    IsDirectory,
    NotADirectory,
    ReadError,
    WriteError,
    GenericError,
}

pub struct DirEntry {
    pub name: String,
    pub size: u64,
    pub cluster: u32, // Internal handle, filesystem-specific
    pub file_type: FileType,
    /// Last write, as a FILETIME (0: not known).
    pub mtime: i64,
}

/// What's at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// An open file as its filesystem keeps it. A filesystem with real partial I/O
/// (FAT) uses the location fields; the default methods keep the whole file in
/// `whole` and write it back on `sync`.
#[derive(Default)]
pub struct FileState {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    /// First cluster, and the clusters in order once `chain_loaded`.
    pub first: u32,
    pub chain: Vec<u32>,
    pub chain_loaded: bool,
    /// Where the directory entry is.
    pub entry_lba: u64,
    pub entry_off: usize,
    pub whole: Option<Vec<u8>>,
    /// Written since the last `sync`.
    pub dirty: bool,
}

// -----| traits |-----

/// Anything that can read/write 512-byte sectors (see `block::Partition`).
/// `buffer_virt` is a kernel virtual address of `count * 512` bytes.
pub trait BlockDevice: Send + Sync {
    fn read_sectors(&self, lba: u64, count: u32, buffer_virt: u64) -> bool;
    fn write_sectors(&self, lba: u64, count: u32, buffer_virt: u64) -> bool;
}

pub trait FileSystem: Send + Sync {
    fn read_file(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<u8>, VfsError>;
    fn list_dir(&self, device: &dyn BlockDevice, path: &str) -> Result<Vec<DirEntry>, VfsError>;
    /// Create or overwrite a file with the given data.
    /// Implementations that do not support writes return Err(VfsError::WriteError).
    fn write_file(&self, device: &dyn BlockDevice, path: &str, data: &[u8]) -> Result<(), VfsError> {
        let _ = (device, path, data);
        Err(VfsError::WriteError)
    }
    /// Delete a file (or an empty directory).
    fn delete_file(&self, device: &dyn BlockDevice, path: &str) -> Result<(), VfsError> {
        let _ = (device, path);
        Err(VfsError::WriteError)
    }
    /// Create a directory. Succeeds if it already exists; the parent must exist.
    fn create_dir(&self, device: &dyn BlockDevice, path: &str) -> Result<(), VfsError> {
        let _ = (device, path);
        Err(VfsError::WriteError)
    }
    /// What's at `path`. The default looks through its parent's listing.
    fn stat(&self, device: &dyn BlockDevice, path: &str) -> Option<Stat> {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() {
            return Some(Stat { is_dir: true, size: 0, mtime: 0 });
        }
        let (dir, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        let listing = self.list_dir(device, if dir.is_empty() { "/" } else { dir }).ok()?;
        listing.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)).map(|e| Stat {
            is_dir: e.file_type == FileType::Directory,
            size: e.size,
            mtime: e.mtime,
        })
    }
    /// Open a file for `read_at` and `write_at`.
    fn open_file(&self, device: &dyn BlockDevice, path: &str) -> Result<FileState, VfsError> {
        let whole = self.read_file(device, path)?;
        Ok(FileState { path: String::from(path), size: whole.len() as u64, whole: Some(whole), ..FileState::default() })
    }
    /// Read at `off`: the bytes read, 0 at or past the end.
    fn read_at(&self, device: &dyn BlockDevice, f: &mut FileState, off: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        let _ = device;
        let whole = f.whole.as_ref().ok_or(VfsError::ReadError)?;
        let start = (off as usize).min(whole.len());
        let n = buf.len().min(whole.len() - start);
        buf[..n].copy_from_slice(&whole[start..start + n]);
        Ok(n)
    }
    /// Write at `off`, growing the file if needed. A gap reads as zeros.
    fn write_at(&self, device: &dyn BlockDevice, f: &mut FileState, off: u64, data: &[u8]) -> Result<(), VfsError> {
        let _ = device;
        let whole = f.whole.as_mut().ok_or(VfsError::WriteError)?;
        let end = off as usize + data.len();
        if whole.len() < end {
            whole.resize(end, 0);
        }
        whole[off as usize..end].copy_from_slice(data);
        f.size = whole.len() as u64;
        f.dirty = true;
        Ok(())
    }
    fn set_len(&self, device: &dyn BlockDevice, f: &mut FileState, len: u64) -> Result<(), VfsError> {
        let _ = device;
        let whole = f.whole.as_mut().ok_or(VfsError::WriteError)?;
        whole.resize(len as usize, 0);
        f.size = len;
        f.dirty = true;
        Ok(())
    }
    /// Everything written so far on the disk.
    fn sync(&self, device: &dyn BlockDevice, f: &mut FileState) -> Result<(), VfsError> {
        if !f.dirty {
            return Ok(());
        }
        f.dirty = false;
        match &f.whole {
            Some(whole) => self.write_file(device, &f.path, whole),
            None => Ok(()),
        }
    }
    /// Set the last-write time. Filesystems that can't keep it ignore this.
    fn set_mtime(&self, device: &dyn BlockDevice, f: &mut FileState, mtime: i64) -> Result<(), VfsError> {
        let _ = device;
        f.mtime = mtime;
        Ok(())
    }
    /// Move a file or directory; `to` must not exist. The default copies a file.
    fn rename(&self, device: &dyn BlockDevice, from: &str, to: &str) -> Result<(), VfsError> {
        let data = self.read_file(device, from)?;
        self.write_file(device, to, &data)?;
        self.delete_file(device, from)
    }
}

// -----| vfs manager |-----

pub struct MountedFileSystem {
    pub path: String,
    pub fs: Box<dyn FileSystem>,
    pub device: Box<dyn BlockDevice>,
}

/// An open file. Reads and writes reach the disk without passing through a
/// copy of the whole file, unless its filesystem works that way.
pub struct VfsFile {
    mount: Arc<MountedFileSystem>,
    state: spin::Mutex<FileState>,
}

impl VfsFile {
    pub fn len(&self) -> u64 {
        self.state.lock().size
    }
    pub fn mtime(&self) -> i64 {
        self.state.lock().mtime
    }
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        self.mount.fs.read_at(&*self.mount.device, &mut self.state.lock(), off, buf)
    }
    pub fn write_at(&self, off: u64, data: &[u8]) -> Result<(), VfsError> {
        self.mount.fs.write_at(&*self.mount.device, &mut self.state.lock(), off, data)
    }
    pub fn set_len(&self, len: u64) -> Result<(), VfsError> {
        self.mount.fs.set_len(&*self.mount.device, &mut self.state.lock(), len)
    }
    pub fn sync(&self) -> Result<(), VfsError> {
        self.mount.fs.sync(&*self.mount.device, &mut self.state.lock())
    }
    pub fn set_mtime(&self, mtime: i64) -> Result<(), VfsError> {
        self.mount.fs.set_mtime(&*self.mount.device, &mut self.state.lock(), mtime)
    }
    /// The file now lives at the volume path of VFS path `path`: look it up
    /// again, as its directory entry has moved.
    pub fn moved(&self, path: &str) -> Result<(), VfsError> {
        let rel = VfsManager::strip_mount_prefix(path, &self.mount.path);
        *self.state.lock() = self.mount.fs.open_file(&*self.mount.device, rel)?;
        Ok(())
    }
}

/// The mount table.
///
/// The lock is held only to find the mount that owns a path; the filesystem
/// call itself runs without it. It used to be one `spin::Mutex<VfsManager>`
/// held across every disk operation, which serialised all file I/O
/// system-wide. Each disk has its own lock now (`block::Disk`).
pub struct VfsManager {
    mounts: spin::Mutex<Vec<Arc<MountedFileSystem>>>,
}

impl VfsManager {
    pub const fn new() -> Self {
        Self { mounts: spin::Mutex::new(Vec::new()) }
    }

    pub fn mount(&self, path: String, fs: Box<dyn FileSystem>, device: Box<dyn BlockDevice>) {
        self.mounts.lock().push(Arc::new(MountedFileSystem { path, fs, device }));
    }

    pub fn mount_count(&self) -> usize {
        self.mounts.lock().len()
    }

    /// Returns true if `path` falls under `mount_point`.
    /// Handles three cases:
    ///   1. Root mount ("/") — matches every absolute path.
    ///   2. Exact match — path IS the mount point (e.g. listing the mount dir itself).
    ///   3. Prefix match — path starts with mount_point followed by '/'
    ///      so "/foo" never incorrectly matches "/foobar/file.txt".
    /// Case-insensitive, like the FAT names underneath (the terminal
    /// upper-cases what it's given, so `/MNT/D1P0` must reach `/mnt/d1p0`).
    fn path_under_mount(path: &str, mount_point: &str) -> bool {
        if mount_point == "/" {
            return true;
        }
        let n = mount_point.len();
        path.len() >= n
            && path.is_char_boundary(n)
            && path[..n].eq_ignore_ascii_case(mount_point)
            && (path.len() == n || path[n..].starts_with('/'))
    }

    /// Strip the mount-point prefix from `path`, returning the filesystem-relative path.
    /// For root mounts the path is returned unchanged (it already starts with '/').
    fn strip_mount_prefix<'a>(path: &'a str, mount_point: &str) -> &'a str {
        if mount_point == "/" {
            path
        } else {
            let stripped = &path[mount_point.len()..];
            if stripped.is_empty() { "/" } else { stripped }
        }
    }

    /// The mount owning `path` (the longest matching mount point, so
    /// `/mnt/d1p0/x` goes to that mount rather than to `/`) and the path
    /// relative to it.
    fn resolve(&self, path: &str) -> Option<(Arc<MountedFileSystem>, String)> {
        let mounts = self.mounts.lock();
        let mount = mounts
            .iter()
            .filter(|m| Self::path_under_mount(path, &m.path))
            .max_by_key(|m| m.path.len())?
            .clone();
        let relative = String::from(Self::strip_mount_prefix(path, &mount.path));
        Some((mount, relative))
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.read_file(&*m.device, &rel),
            None => {
                crate::serial_println!("[VFS] read_file: no mount matched for '{}'", path);
                Err(VfsError::FileNotFound)
            }
        }
    }

    /// List a directory. Mount points that sit directly inside it (e.g.
    /// `mnt` in `/`, `d1p0` in `/mnt`) are listed too, even when the parent
    /// filesystem has no such directory.
    pub fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        let listed = self.resolve(path).map(|(m, rel)| m.fs.list_dir(&*m.device, &rel));
        let children = self.mount_children(path);
        let as_dir = |name: String| DirEntry { name, size: 0, cluster: 0, file_type: FileType::Directory, mtime: 0 };
        match listed {
            Some(Ok(mut entries)) => {
                for name in children {
                    if !entries.iter().any(|e| e.name.eq_ignore_ascii_case(&name)) {
                        entries.push(as_dir(name));
                    }
                }
                Ok(entries)
            }
            _ if !children.is_empty() => Ok(children.into_iter().map(as_dir).collect()),
            Some(Err(e)) => Err(e),
            None => Err(VfsError::FileNotFound),
        }
    }

    /// Names of the next path component of every mount point below `dir`.
    fn mount_children(&self, dir: &str) -> Vec<String> {
        let prefix = if dir.ends_with('/') { String::from(dir) } else { alloc::format!("{}/", dir) };
        let mut names: Vec<String> = Vec::new();
        for m in self.mounts.lock().iter() {
            let rest = (m.path.len() > prefix.len() && m.path[..prefix.len()].eq_ignore_ascii_case(&prefix))
                .then(|| &m.path[prefix.len()..]);
            if let Some(rest) = rest {
                if let Some(first) = rest.split('/').next().filter(|s| !s.is_empty()) {
                    if !names.iter().any(|n| n == first) {
                        names.push(String::from(first));
                    }
                }
            }
        }
        names
    }

    /// What's at `path`; a mount point counts as a directory.
    pub fn stat(&self, path: &str) -> Option<Stat> {
        if let Some(s) = self.resolve(path).and_then(|(m, rel)| m.fs.stat(&*m.device, &rel)) {
            return Some(s);
        }
        let trimmed = path.trim_end_matches('/');
        let (dir, name) = trimmed.rsplit_once('/')?;
        let dir = if dir.is_empty() { "/" } else { dir };
        self.mount_children(dir).iter().any(|c| c.eq_ignore_ascii_case(name)).then_some(Stat { is_dir: true, size: 0, mtime: 0 })
    }

    pub fn open(&self, path: &str) -> Result<VfsFile, VfsError> {
        let (mount, rel) = self.resolve(path).ok_or(VfsError::FileNotFound)?;
        let state = mount.fs.open_file(&*mount.device, &rel)?;
        Ok(VfsFile { mount, state: spin::Mutex::new(state) })
    }

    /// Move a file or directory within one volume; `to` must not exist.
    /// GenericError means the two paths are on different volumes.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), VfsError> {
        let (m, f) = self.resolve(from).ok_or(VfsError::FileNotFound)?;
        let (n, t) = self.resolve(to).ok_or(VfsError::FileNotFound)?;
        if !Arc::ptr_eq(&m, &n) {
            return Err(VfsError::GenericError);
        }
        m.fs.rename(&*m.device, &f, &t)
    }

    /// Create or overwrite a file at `path`. Routes to the mounted filesystem
    /// that owns the path prefix, then delegates to its `write_file` impl.
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.write_file(&*m.device, &rel, data),
            None => Err(VfsError::FileNotFound),
        }
    }

    /// Delete a file (or empty directory) at `path`. Routes to the mounted filesystem.
    pub fn delete_file(&self, path: &str) -> Result<(), VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.delete_file(&*m.device, &rel),
            None => Err(VfsError::FileNotFound),
        }
    }

    /// Create a directory at `path` (its parent must exist).
    pub fn create_dir(&self, path: &str) -> Result<(), VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.create_dir(&*m.device, &rel),
            None => Err(VfsError::FileNotFound),
        }
    }
}

pub static VFS: VfsManager = VfsManager::new();

/// Directories the kernel writes into (`/etc/users.vdb`, `/tmp/...`
/// checkpoints, `/net/...`, `/home/<user>`). Created on the root volume at
/// boot if missing; a read-only root just leaves them absent.
pub fn ensure_standard_dirs() {
    for dir in ["/etc", "/tmp", "/var", "/net", "/home"] {
        if let Err(e) = VFS.create_dir(dir) {
            crate::serial_println!("[VFS] could not create {}: {:?}", dir, e);
        }
    }
}
