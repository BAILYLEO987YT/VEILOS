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
    /// Delete a file.
    fn delete_file(&self, device: &dyn BlockDevice, path: &str) -> Result<(), VfsError> {
        let _ = (device, path);
        Err(VfsError::WriteError)
    }
}

// -----| vfs manager |-----

pub struct MountedFileSystem {
    pub path: String,
    pub fs: Box<dyn FileSystem>,
    pub device: Box<dyn BlockDevice>,
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
    fn path_under_mount(path: &str, mount_point: &str) -> bool {
        if mount_point == "/" {
            return true;
        }
        if path == mount_point {
            return true;
        }
        if path.starts_with(mount_point) {
            let rest = &path[mount_point.len()..];
            return rest.starts_with('/');
        }
        false
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
        let as_dir = |name: String| DirEntry { name, size: 0, cluster: 0, file_type: FileType::Directory };
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
            if let Some(rest) = m.path.strip_prefix(prefix.as_str()) {
                if let Some(first) = rest.split('/').next().filter(|s| !s.is_empty()) {
                    if !names.iter().any(|n| n == first) {
                        names.push(String::from(first));
                    }
                }
            }
        }
        names
    }

    /// Create or overwrite a file at `path`. Routes to the mounted filesystem
    /// that owns the path prefix, then delegates to its `write_file` impl.
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.write_file(&*m.device, &rel, data),
            None => Err(VfsError::FileNotFound),
        }
    }

    /// Delete a file at `path`. Routes to the mounted filesystem.
    pub fn delete_file(&self, path: &str) -> Result<(), VfsError> {
        match self.resolve(path) {
            Some((m, rel)) => m.fs.delete_file(&*m.device, &rel),
            None => Err(VfsError::FileNotFound),
        }
    }
}

pub static VFS: VfsManager = VfsManager::new();
