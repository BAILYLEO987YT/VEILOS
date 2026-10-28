use alloc::string::String;
use alloc::vec::Vec;
use alloc::boxed::Box;

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

/// Abstract trait for any hardware that can read/write raw sectors.
pub trait BlockDevice: Send + Sync {
    fn read_sectors(&self, lba: u64, count: u32, buffer_phys: u64) -> bool;
    fn write_sectors(&self, lba: u64, count: u32, buffer_phys: u64) -> bool;
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

pub struct VfsManager {
    pub mounts: Vec<MountedFileSystem>,
}

impl VfsManager {
    pub const fn new() -> Self {
        Self {
            mounts: Vec::new(),
        }
    }

    pub fn mount(&mut self, path: String, fs: Box<dyn FileSystem>, device: Box<dyn BlockDevice>) {
        self.mounts.push(MountedFileSystem {
            path,
            fs,
            device,
        });
    }

    /// Returns true if `path` falls under `mount_point`.
    /// Handles three cases:
    ///   1. Root mount ("/") — matches every absolute path.
    ///   2. Exact match — path IS the mount point (e.g. listing the mount dir itself).
    ///   3. Prefix match — path starts with mount_point followed by '/'
    ///      so "/foo" never incorrectly matches "/foobar/file.txt".
    fn path_under_mount(path: &str, mount_point: &str) -> bool {
        // Case 1: root mount matches everything.
        if mount_point == "/" {
            return true;
        }
        // Case 2: exact match (e.g. listing the mount root itself).
        if path == mount_point {
            return true;
        }
        // Case 3: path is inside mount_point — must have '/' separator.
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
            path  // e.g. "/config.vsh" → "/config.vsh" (FAT32 resolve_path handles the leading '/')
        } else {
            let stripped = &path[mount_point.len()..];
            if stripped.is_empty() { "/" } else { stripped }
        }
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        for mount in &self.mounts {
            if Self::path_under_mount(path, &mount.path) {
                let relative_path = Self::strip_mount_prefix(path, &mount.path);
                return mount.fs.read_file(&*mount.device, relative_path);
            }
        }
        crate::serial_println!("[VFS] read_file: no mount matched for '{}'", path);
        Err(VfsError::FileNotFound)
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, VfsError> {
        for mount in &self.mounts {
            if Self::path_under_mount(path, &mount.path) {
                let relative_path = Self::strip_mount_prefix(path, &mount.path);
                return mount.fs.list_dir(&*mount.device, relative_path);
            }
        }
        Err(VfsError::FileNotFound)
    }

    /// Create or overwrite a file at `path`. Routes to the mounted filesystem
    /// that owns the path prefix, then delegates to its `write_file` impl.
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), VfsError> {
        for mount in &self.mounts {
            if Self::path_under_mount(path, &mount.path) {
                let relative_path = Self::strip_mount_prefix(path, &mount.path);
                return mount.fs.write_file(&*mount.device, relative_path, data);
            }
        }
        Err(VfsError::FileNotFound)
    }

    /// Delete a file at `path`. Routes to the mounted filesystem.
    pub fn delete_file(&self, path: &str) -> Result<(), VfsError> {
        for mount in &self.mounts {
            if Self::path_under_mount(path, &mount.path) {
                let relative_path = Self::strip_mount_prefix(path, &mount.path);
                return mount.fs.delete_file(&*mount.device, relative_path);
            }
        }
        Err(VfsError::FileNotFound)
    }
}

pub static VFS: spin::Mutex<VfsManager> = spin::Mutex::new(VfsManager::new());
