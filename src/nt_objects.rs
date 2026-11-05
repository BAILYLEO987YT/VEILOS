// -----| VEILOS NT Object Manager |-----
//
// Implements the NT Object Model that underpins Windows application compatibility:
//   - Reference-counted kernel objects (Events, Mutexes, Semaphores, Sections)
//   - Per-process handle tables (opaque Handle -> Arc<KernelObject> mapping)
//   - A hierarchical object namespace mirroring the NT namespace (\Device\, \BaseNamedObjects\, etc.)
//
// Phase 5 roadmap items implemented here:
//   [x] VEILOS Object Manager analogue
//   [x] Unified handle tables for processes
//   [x] Namespaces for devices, files, events, pipes, etc.
//   [x] Reference-counted kernel objects (events, mutexes, semaphores, sections)

#![allow(dead_code)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use spin::Mutex;
use crate::println;

// =====================================================================
// -----| Object Types |-----
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Event,
    Mutex,      // NT terminology: "Mutant"
    Semaphore,
    Section,    // Shared memory / memory-mapped file
    File,
    Directory,  // Namespace directory node
    Process,
    Thread,
    Console,
}

// =====================================================================
// -----| Kernel Object Trait |-----
// =====================================================================

/// The root trait for all kernel objects. All kernel objects are:
///   - Heap-allocated behind Arc<> for shared ownership
///   - Send + Sync so they can be referenced from any CPU context
///   - Identified by an ObjectType
pub trait KernelObject: Send + Sync {
    fn obj_type(&self) -> ObjectType;
    fn debug_name(&self) -> &str { "<unnamed>" }
}

// =====================================================================
// -----| Event Object |-----
// =====================================================================
//
// NT Event objects come in two flavours:
//   NotificationEvent (auto_reset=false) — stays signaled until manually reset.
//                                          All waiters are released at once.
//   SynchronizationEvent (auto_reset=true) — auto-resets after releasing ONE waiter.

pub struct EventObject {
    pub name: Option<String>,
    pub auto_reset: bool,
    pub signaled: AtomicBool,
}

impl EventObject {
    pub fn new(name: Option<String>, auto_reset: bool, initial_state: bool) -> Self {
        Self { name, auto_reset, signaled: AtomicBool::new(initial_state) }
    }

    /// Signal the event. If auto_reset, exactly one waiter will observe the signal.
    pub fn set(&self) {
        self.signaled.store(true, Ordering::Release);
    }

    /// Manually reset the event to non-signaled.
    pub fn reset(&self) {
        self.signaled.store(false, Ordering::Release);
    }

    /// Test the event. If auto_reset and signaled, consumes the signal atomically.
    pub fn test_and_consume(&self) -> bool {
        if !self.signaled.load(Ordering::Acquire) { return false; }
        if self.auto_reset {
            // Atomically consume the signal so only one caller gets it.
            self.signaled.compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed).is_ok()
        } else {
            true
        }
    }
}

impl KernelObject for EventObject {
    fn obj_type(&self) -> ObjectType { ObjectType::Event }
    fn debug_name(&self) -> &str { self.name.as_deref().unwrap_or("<unnamed event>") }
}

// =====================================================================
// -----| Mutex Object (NT: Mutant) |-----
// =====================================================================
//
// NT mutexes are recursive — the same thread (process in our simplified model)
// can acquire the same mutex multiple times without deadlocking.
// The mutex is released only when the acquisition count drops to zero.

pub struct MutexObject {
    pub name: Option<String>,
    pub locked: AtomicBool,
    pub owner_pid: AtomicU32,
    pub recursion_count: AtomicU32,
}

impl MutexObject {
    pub fn new(name: Option<String>, initially_owned: bool) -> Self {
        Self {
            name,
            locked: AtomicBool::new(initially_owned),
            owner_pid: AtomicU32::new(0),
            recursion_count: AtomicU32::new(if initially_owned { 1 } else { 0 }),
        }
    }

    /// Who a Windows mutex belongs to: the running thread, not the process
    /// (`owner_pid` then holds this).
    pub fn current_owner() -> u32 {
        (crate::kthread::current_user() as usize >> 4) as u32 | 1
    }

    /// Attempt a non-blocking acquisition for the given process.
    /// Returns true if the lock was acquired (or was already owned by this pid).
    pub fn try_acquire(&self, pid: u32) -> bool {
        let currently_locked = self.locked.load(Ordering::Acquire);

        if currently_locked {
            // Recursive acquisition by the same owner is always allowed.
            if self.owner_pid.load(Ordering::Acquire) == pid {
                self.recursion_count.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            return false; // Held by someone else
        }

        // Try to atomically take the lock.
        match self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => {
                self.owner_pid.store(pid, Ordering::Release);
                self.recursion_count.store(1, Ordering::Release);
                true
            }
            Err(_) => false, // Lost the race
        }
    }

    /// Release one acquisition level. Returns Err if not owned by this pid.
    pub fn release(&self, pid: u32) -> Result<u32, &'static str> {
        if !self.locked.load(Ordering::Acquire) {
            return Err("NtReleaseMutant: mutex is not locked");
        }
        if self.owner_pid.load(Ordering::Acquire) != pid {
            return Err("NtReleaseMutant: caller does not own this mutex");
        }
        let prev_count = self.recursion_count.fetch_sub(1, Ordering::AcqRel);
        if prev_count == 1 {
            // Last release — unlock completely.
            self.owner_pid.store(0, Ordering::Release);
            self.locked.store(false, Ordering::Release);
        }
        Ok(prev_count)
    }

    pub fn is_locked(&self) -> bool { self.locked.load(Ordering::Acquire) }
    pub fn owner(&self) -> u32 { self.owner_pid.load(Ordering::Acquire) }
    pub fn recursion(&self) -> u32 { self.recursion_count.load(Ordering::Acquire) }
}

impl KernelObject for MutexObject {
    fn obj_type(&self) -> ObjectType { ObjectType::Mutex }
    fn debug_name(&self) -> &str { self.name.as_deref().unwrap_or("<unnamed mutex>") }
}

// =====================================================================
// -----| Semaphore Object |-----
// =====================================================================
//
// Classic counting semaphore with a configurable ceiling.
// acquire() decrements the count (returns false if at zero — caller must wait).
// release() increments the count up to max_count.

pub struct SemaphoreObject {
    pub name: Option<String>,
    pub count: AtomicI32,
    pub max_count: i32,
}

impl SemaphoreObject {
    pub fn new(name: Option<String>, initial_count: i32, max_count: i32) -> Self {
        Self { name, count: AtomicI32::new(initial_count), max_count }
    }

    /// Non-blocking acquire. Returns false if the semaphore is at zero.
    pub fn try_acquire(&self) -> bool {
        let mut current = self.count.load(Ordering::Acquire);
        loop {
            if current <= 0 { return false; }
            match self.count.compare_exchange(current, current - 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return true,
                Err(actual) => current = actual, // Retry with the freshly observed value
            }
        }
    }

    /// Release `release_count` units. Returns the previous count, or Err on overflow.
    pub fn release(&self, release_count: i32) -> Result<i32, &'static str> {
        let prev = self.count.fetch_add(release_count, Ordering::AcqRel);
        if prev + release_count > self.max_count {
            // Overflow — undo and report error.
            self.count.fetch_sub(release_count, Ordering::Relaxed);
            return Err("NtReleaseSemaphore: count would exceed maximum");
        }
        Ok(prev)
    }

    pub fn current_count(&self) -> i32 { self.count.load(Ordering::Acquire) }
}

impl KernelObject for SemaphoreObject {
    fn obj_type(&self) -> ObjectType { ObjectType::Semaphore }
    fn debug_name(&self) -> &str { self.name.as_deref().unwrap_or("<unnamed semaphore>") }
}

// =====================================================================
// -----| Section Object (Shared Memory / Memory-Mapped File) |-----
// =====================================================================
//
// NT Section objects are the primitive behind CreateFileMapping / MapViewOfFile.
// Here we model them as a named byte buffer; memory-mapping into process address
// space is handled by the Windows Memory Manager Personality (future phase).

pub struct SectionObject {
    pub name: Option<String>,
    pub size: usize,
    pub page_protection: u32, // PAGE_READONLY (0x02), PAGE_READWRITE (0x04), etc.
    pub data: Mutex<Vec<u8>>,
}

impl SectionObject {
    pub fn new(name: Option<String>, size: usize, page_protection: u32) -> Self {
        let mut backing = Vec::new();
        backing.resize(size, 0u8);
        Self { name, size, page_protection, data: Mutex::new(backing) }
    }

    pub fn read_bytes(&self, offset: usize, buf: &mut [u8]) -> Result<(), &'static str> {
        let data = self.data.lock();
        if offset + buf.len() > data.len() { return Err("Section read out of bounds"); }
        buf.copy_from_slice(&data[offset..offset + buf.len()]);
        Ok(())
    }

    pub fn write_bytes(&self, offset: usize, src: &[u8]) -> Result<(), &'static str> {
        if self.page_protection == PAGE_READONLY { return Err("Section is read-only"); }
        let mut data = self.data.lock();
        if offset + src.len() > data.len() { return Err("Section write out of bounds"); }
        data[offset..offset + src.len()].copy_from_slice(src);
        Ok(())
    }
}

pub const PAGE_READONLY:   u32 = 0x02;
pub const PAGE_READWRITE:  u32 = 0x04;
pub const PAGE_EXECUTE:    u32 = 0x10;
pub const PAGE_EXECUTE_RW: u32 = 0x40;

impl KernelObject for SectionObject {
    fn obj_type(&self) -> ObjectType { ObjectType::Section }
    fn debug_name(&self) -> &str { self.name.as_deref().unwrap_or("<unnamed section>") }
}

// =====================================================================
// -----| Handle Table |-----
// =====================================================================
//
// Each process owns a HandleTable. A Handle is just a u32. Handle 0 is
// always INVALID_HANDLE_VALUE. All other values are 1-based indices into
// the process's private entry array.
//
// We avoid the `Any` / `downcast` pattern (which requires std) by storing
// the concrete object type as a `KernelObjectRef` enum alongside the
// access mask. Typed accessors simply pattern-match on this enum.

pub type Handle = u32;
pub const INVALID_HANDLE_VALUE: Handle = 0xFFFFFFFF;

/// Typed wrapper so we can recover concrete Arc<T> without Any/downcast.
#[derive(Clone)]
pub enum KernelObjectRef {
    Event(Arc<EventObject>),
    Mutex(Arc<MutexObject>),
    Semaphore(Arc<SemaphoreObject>),
    Section(Arc<SectionObject>),
    Console(Arc<crate::console::Console>),
    File(Arc<crate::nt_file::FileObject>),
    Process(Arc<crate::process::Process>),
    Thread(Arc<crate::nt_process::ThreadObj>),
    Pipe(Arc<crate::nt_process::PipeEnd>),
    Duplex(Arc<crate::nt_ipc::DuplexEnd>),
    /// The NUL device: reads end at once, writes go nowhere.
    Null,
    /// A Winsock socket (nt_socket.rs).
    Socket(Arc<crate::nt_socket::SocketObj>),
}

impl KernelObjectRef {
    pub fn obj_type(&self) -> ObjectType {
        match self {
            Self::Event(_)     => ObjectType::Event,
            Self::Mutex(_)     => ObjectType::Mutex,
            Self::Semaphore(_) => ObjectType::Semaphore,
            Self::Section(_)   => ObjectType::Section,
            Self::Console(_)   => ObjectType::Console,
            Self::File(_)      => ObjectType::File,
            Self::Process(_)   => ObjectType::Process,
            Self::Thread(_)    => ObjectType::Thread,
            Self::Pipe(_)      => ObjectType::File,
            Self::Duplex(_)    => ObjectType::File,
            Self::Null         => ObjectType::File,
            Self::Socket(_)    => ObjectType::File,
        }
    }
}

/// In `HandleEntry::attributes`: SetHandleInformation cleared
/// HANDLE_FLAG_INHERIT (handles are inheritable unless told otherwise).
pub const HANDLE_NO_INHERIT: u32 = 0x8000_0000;

#[derive(Clone)]
pub struct HandleEntry {
    pub object:      KernelObjectRef,
    pub access_mask: u32,
    pub attributes:  u32,
}

pub struct HandleTable {
    /// entries[0] is always None — handle 0 is reserved as invalid.
    entries: Vec<Option<HandleEntry>>,
}

impl HandleTable {
    pub fn new() -> Self {
        let mut entries = Vec::new();
        entries.push(None); // Slot 0 = invalid
        Self { entries }
    }

    /// Allocate a new handle for the given object. Returns the opaque handle value.
    pub fn allocate_ref(&mut self, object: KernelObjectRef, access_mask: u32, attributes: u32) -> Handle {
        for (i, slot) in self.entries.iter_mut().enumerate().skip(1) {
            if slot.is_none() {
                *slot = Some(HandleEntry { object, access_mask, attributes });
                return i as Handle;
            }
        }
        let handle = self.entries.len() as Handle;
        self.entries.push(Some(HandleEntry { object, access_mask, attributes }));
        handle
    }

    /// Convenience: allocate an event handle.
    pub fn allocate_event(&mut self, obj: Arc<EventObject>, access_mask: u32) -> Handle {
        self.allocate_ref(KernelObjectRef::Event(obj), access_mask, 0)
    }
    pub fn allocate_mutex(&mut self, obj: Arc<MutexObject>, access_mask: u32) -> Handle {
        self.allocate_ref(KernelObjectRef::Mutex(obj), access_mask, 0)
    }
    pub fn allocate_semaphore(&mut self, obj: Arc<SemaphoreObject>, access_mask: u32) -> Handle {
        self.allocate_ref(KernelObjectRef::Semaphore(obj), access_mask, 0)
    }
    pub fn allocate_section(&mut self, obj: Arc<SectionObject>, access_mask: u32) -> Handle {
        self.allocate_ref(KernelObjectRef::Section(obj), access_mask, 0)
    }

    /// A copy of every inheritable handle, at the same values.
    pub fn duplicate_all(&self) -> Self {
        let entries = self
            .entries
            .iter()
            .map(|e| e.as_ref().filter(|e| e.attributes & HANDLE_NO_INHERIT == 0).cloned())
            .collect();
        Self { entries }
    }

    /// A process this table holds a handle to.
    pub fn find_process(&self, pid: u32) -> Option<Arc<crate::process::Process>> {
        self.entries.iter().flatten().find_map(|e| match &e.object {
            KernelObjectRef::Process(p) if p.pid == pid => Some(p.clone()),
            _ => None,
        })
    }

    /// Whether children get a copy of `handle` (HANDLE_FLAG_INHERIT).
    pub fn inheritable(&self, handle: Handle) -> Option<bool> {
        let e = self.entries.get(handle as usize)?.as_ref()?;
        Some(e.attributes & HANDLE_NO_INHERIT == 0)
    }

    pub fn set_inheritable(&mut self, handle: Handle, inherit: bool) -> bool {
        match self.entries.get_mut(handle as usize).and_then(|e| e.as_mut()) {
            Some(e) => {
                if inherit {
                    e.attributes &= !HANDLE_NO_INHERIT;
                } else {
                    e.attributes |= HANDLE_NO_INHERIT;
                }
                true
            }
            None => false,
        }
    }

    /// Get the raw enum ref for any handle.
    pub fn get_ref(&self, handle: Handle) -> Option<&KernelObjectRef> {
        let idx = handle as usize;
        if idx == 0 || idx >= self.entries.len() { return None; }
        self.entries[idx].as_ref().map(|e| &e.object)
    }

    /// Typed accessors — return None on wrong type or invalid handle.
    pub fn as_event(&self, handle: Handle) -> Option<Arc<EventObject>> {
        match self.get_ref(handle)? {
            KernelObjectRef::Event(e) => Some(e.clone()),
            _ => None,
        }
    }
    pub fn as_mutex(&self, handle: Handle) -> Option<Arc<MutexObject>> {
        match self.get_ref(handle)? {
            KernelObjectRef::Mutex(m) => Some(m.clone()),
            _ => None,
        }
    }
    pub fn as_semaphore(&self, handle: Handle) -> Option<Arc<SemaphoreObject>> {
        match self.get_ref(handle)? {
            KernelObjectRef::Semaphore(s) => Some(s.clone()),
            _ => None,
        }
    }
    pub fn as_section(&self, handle: Handle) -> Option<Arc<SectionObject>> {
        match self.get_ref(handle)? {
            KernelObjectRef::Section(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Return the ObjectType for a live handle without cloning.
    pub fn get_type(&self, handle: Handle) -> Option<ObjectType> {
        Some(self.get_ref(handle)?.obj_type())
    }

    /// Check if a handle grants a specific access right.
    pub fn check_access(&self, handle: Handle, required: u32) -> bool {
        let idx = handle as usize;
        if idx == 0 || idx >= self.entries.len() { return false; }
        self.entries[idx].as_ref().map_or(false, |e| e.access_mask & required == required)
    }

    /// Close a handle, dropping the Arc and decrementing the refcount.
    pub fn close(&mut self, handle: Handle) -> bool {
        let idx = handle as usize;
        if idx == 0 || idx >= self.entries.len() { return false; }
        let was_some = self.entries[idx].is_some();
        self.entries[idx] = None;
        was_some
    }

    /// Close every handle in the table.
    ///
    /// Called during process teardown — without it the object manager kept a
    /// reference to every kernel object the dead process ever opened, so
    /// events/mutants/sections were never actually released.
    pub fn close_all(&mut self) -> usize {
        let mut closed = 0usize;
        for slot in self.entries.iter_mut() {
            if slot.take().is_some() { closed += 1; }
        }
        closed
    }

    pub fn live_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
}

// =====================================================================
// -----| Object Namespace |-----
// =====================================================================
//
// The NT namespace is a directory tree — names only, no stored objects.
// Object lookup goes through HandleTable; the namespace is for listing.

pub struct NamespaceNode {
    pub name:     String,
    pub children: Vec<NamespaceNode>,
}

impl NamespaceNode {
    pub fn dir(name: &str) -> Self {
        Self { name: String::from(name), children: Vec::new() }
    }

    fn child(&self, name: &str) -> Option<&NamespaceNode> {
        self.children.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn child_mut(&mut self, name: &str) -> Option<&mut NamespaceNode> {
        self.children.iter_mut().find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

pub struct ObjectNamespace {
    root: NamespaceNode,
}

impl ObjectNamespace {
    pub fn new() -> Self {
        let mut root = NamespaceNode::dir("\\");

        let mut device = NamespaceNode::dir("Device");
        device.children.push(NamespaceNode::dir("HarddiskVolume0"));
        device.children.push(NamespaceNode::dir("Keyboard0"));
        device.children.push(NamespaceNode::dir("Pipe"));
        device.children.push(NamespaceNode::dir("Null"));
        device.children.push(NamespaceNode::dir("Console"));
        root.children.push(device);

        root.children.push(NamespaceNode::dir("BaseNamedObjects"));
        root.children.push(NamespaceNode::dir("Sessions"));
        root.children.push(NamespaceNode::dir("KernelObjects"));
        root.children.push(NamespaceNode::dir("??"));

        Self { root }
    }

    fn split_path(path: &str) -> Vec<&str> {
        path.split('\\').filter(|s| !s.is_empty()).collect()
    }

    /// List the children of a directory node.
    pub fn list_dir(&self, path: &str) -> Vec<&str> {
        let segs = Self::split_path(path);
        let mut node = &self.root;
        for seg in &segs {
            match node.child(seg) {
                Some(n) => node = n,
                None    => return Vec::new(),
            }
        }
        node.children.iter().map(|c| c.name.as_str()).collect()
    }

    /// Register a named entry (creates intermediate dirs as needed).
    pub fn insert(&mut self, path: &str) -> Result<(), &'static str> {
        let segs = Self::split_path(path);
        if segs.is_empty() { return Err("Empty namespace path"); }
        let mut node = &mut self.root;
        for seg in &segs[..segs.len() - 1] {
            if node.child(seg).is_none() {
                node.children.push(NamespaceNode::dir(seg));
            }
            node = node.child_mut(seg).unwrap();
        }
        let leaf = segs.last().unwrap();
        if node.child(leaf).is_none() {
            node.children.push(NamespaceNode::dir(leaf));
        }
        Ok(())
    }

    /// Remove a named entry.
    pub fn remove(&mut self, path: &str) -> bool {
        let segs = Self::split_path(path);
        if segs.is_empty() { return false; }
        let mut node = &mut self.root;
        for seg in &segs[..segs.len() - 1] {
            match node.child_mut(seg) {
                Some(n) => node = n,
                None    => return false,
            }
        }
        let leaf = segs.last().unwrap();
        let before = node.children.len();
        node.children.retain(|c| !c.name.eq_ignore_ascii_case(leaf));
        node.children.len() < before
    }
}

// =====================================================================
// -----| Object Manager |-----
// =====================================================================
//
// The ObjectManager is the single authority for creating and looking up
// kernel objects. It owns the namespace tree and provides factory methods
// that both construct the object and register it if a name is supplied.

pub struct ObjectManager {
    pub namespace: ObjectNamespace,
    next_obj_id:   u64,
}

impl ObjectManager {
    pub const fn new() -> Self {
        Self {
            namespace:   ObjectNamespace { root: NamespaceNode {
                name:     String::new(),
                children: Vec::new(),
            }},
            next_obj_id: 1,
        }
    }

    /// Call once from kernel_main to populate the namespace tree.
    pub fn init(&mut self) {
        self.namespace = ObjectNamespace::new();
        println!("[OBJMGR] Namespace initialised. Standard NT directories created.");
        println!("[OBJMGR]   \\Device\\       — Hardware device objects");
        println!("[OBJMGR]   \\BaseNamedObjects\\ — User-visible named sync primitives");
        println!("[OBJMGR]   \\??\\            — DOS device aliases");
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_obj_id;
        self.next_obj_id += 1;
        id
    }

    // ----- Factory: Event -----

    pub fn create_event(
        &mut self,
        name: Option<&str>,
        auto_reset: bool,
        initial_state: bool,
    ) -> Arc<EventObject> {
        let obj = Arc::new(EventObject::new(name.map(String::from), auto_reset, initial_state));
        if let Some(n) = name {
            let path = alloc::format!("\\BaseNamedObjects\\{}", n);
            let _ = self.namespace.insert(&path);
        }
        println!("[OBJMGR] CreateEvent: {} auto_reset={} initial_state={}",
            name.unwrap_or("<anon>"), auto_reset, initial_state);
        obj
    }

    // ----- Factory: Mutex -----

    pub fn create_mutex(
        &mut self,
        name: Option<&str>,
        initially_owned: bool,
    ) -> Arc<MutexObject> {
        let obj = Arc::new(MutexObject::new(name.map(String::from), initially_owned));
        if let Some(n) = name {
            let path = alloc::format!("\\BaseNamedObjects\\{}", n);
            let _ = self.namespace.insert(&path);
        }
        println!("[OBJMGR] CreateMutex: {} initially_owned={}",
            name.unwrap_or("<anon>"), initially_owned);
        obj
    }

    // ----- Factory: Semaphore -----

    pub fn create_semaphore(
        &mut self,
        name: Option<&str>,
        initial_count: i32,
        max_count: i32,
    ) -> Arc<SemaphoreObject> {
        let obj = Arc::new(SemaphoreObject::new(name.map(String::from), initial_count, max_count));
        if let Some(n) = name {
            let path = alloc::format!("\\BaseNamedObjects\\{}", n);
            let _ = self.namespace.insert(&path);
        }
        println!("[OBJMGR] CreateSemaphore: {} count={}/{}",
            name.unwrap_or("<anon>"), initial_count, max_count);
        obj
    }

    // ----- Factory: Section -----

    pub fn create_section(
        &mut self,
        name: Option<&str>,
        size: usize,
        page_protection: u32,
    ) -> Arc<SectionObject> {
        let obj = Arc::new(SectionObject::new(name.map(String::from), size, page_protection));
        if let Some(n) = name {
            let path = alloc::format!("\\BaseNamedObjects\\{}", n);
            let _ = self.namespace.insert(&path);
        }
        println!("[OBJMGR] CreateSection: {} size={} prot={:#x}",
            name.unwrap_or("<anon>"), size, page_protection);
        obj
    }

}

// Global singleton — initialised lazily by init().
pub static OBJECT_MANAGER: Mutex<ObjectManager> = Mutex::new(ObjectManager::new());

/// Call from kernel_main after the heap is ready.
pub fn init() {
    OBJECT_MANAGER.lock().init();
}

// =====================================================================
// -----| NT Status Codes |-----
// =====================================================================

pub const STATUS_SUCCESS:                u64 = 0x00000000;
pub const STATUS_PENDING:                u64 = 0x00000103;
pub const STATUS_TIMEOUT:                u64 = 0x00000102;
pub const STATUS_OBJECT_NAME_EXISTS:     u64 = 0x40000000;
pub const STATUS_INVALID_HANDLE:         u64 = 0xC0000008;
pub const STATUS_ACCESS_DENIED:          u64 = 0xC0000022;
pub const STATUS_OBJECT_NAME_NOT_FOUND:  u64 = 0xC0000034;
pub const STATUS_OBJECT_NAME_COLLISION:  u64 = 0xC0000035;
pub const STATUS_NOT_IMPLEMENTED:        u64 = 0xC0000002;
pub const STATUS_INVALID_PARAMETER:      u64 = 0xC000000D;
pub const STATUS_NO_MEMORY:              u64 = 0xC0000017;
pub const STATUS_SEMAPHORE_LIMIT:        u64 = 0xC0000047;
pub const STATUS_MUTANT_NOT_OWNED:       u64 = 0xC0000046;

// =====================================================================
// -----| Access Masks |-----
// =====================================================================

pub const SYNCHRONIZE:             u32 = 0x00100000;
pub const EVENT_MODIFY_STATE:      u32 = 0x0002;
pub const EVENT_ALL_ACCESS:        u32 = 0x1F0003;
pub const MUTANT_QUERY_STATE:      u32 = 0x0001;
pub const MUTANT_ALL_ACCESS:       u32 = 0x1F0001;
pub const SEMAPHORE_MODIFY_STATE:  u32 = 0x0002;
pub const SEMAPHORE_ALL_ACCESS:    u32 = 0x1F0003;
pub const SECTION_QUERY:           u32 = 0x0001;
pub const SECTION_MAP_WRITE:       u32 = 0x0002;
pub const SECTION_MAP_READ:        u32 = 0x0004;
pub const SECTION_ALL_ACCESS:      u32 = 0x1F01FF;

// =====================================================================
// -----| NT Syscall Dispatcher |-----
// =====================================================================
//
// This free function is called by the WindowsSubsystem syscall handler
// for any syscall ID that maps to an Object Manager operation.
// Keeping this logic here prevents subsystem.rs from growing unboundedly.
//
// Simplified arg conventions (real NT passes pointer-heavy structs):
//   NtCreateEvent    (0x48): arg0=name_ptr|0, arg1=auto_reset, arg2=init_state
//   NtOpenEvent      (0x78): arg0=name_ptr    (currently stubbed — returns INVALID)
//   NtSetEvent       (0x0E): arg0=handle
//   NtClearEvent     (0xCE): arg0=handle
//   NtCreateMutant   (0x5C): arg0=name_ptr|0, arg1=initial_owner, arg2=_
//   NtReleaseMutant  (0xAB): arg0=handle
//   NtCreateSemaphore(0x5E): arg0=name_ptr|0, arg1=init_count,   arg2=max_count
//   NtReleaseSemaphore(0xAC): arg0=handle,    arg1=release_count, arg2=_
//   NtCreateSection  (0x4A): arg0=name_ptr|0, arg1=size,         arg2=protection
//   NtClose          (0x0F): arg0=handle
//   NtWaitForSingleObject(0x04): arg0=handle, arg1=timeout_ms (0=poll), arg2=_
//
// Return value is an NTSTATUS code packed into u64.

pub fn dispatch_nt_object_syscall(syscall_id: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    // We need the caller's handle table — get it through the active process.
    use crate::process::PROCESSOR;

    let process_arc = match PROCESSOR.lock().get_active() {
        Some(p) => p,
        None => {
            println!("[OBJMGR] NT syscall {:#x} called with no active process!", syscall_id);
            return STATUS_ACCESS_DENIED;
        }
    };
    let pid = process_arc.pid;

    match syscall_id {

        // ----- NtCreateEvent -----
        0x48 => {
            let auto_reset    = arg1 != 0;
            let initial_state = arg2 != 0;
            let obj = OBJECT_MANAGER.lock().create_event(None, auto_reset, initial_state);
            let handle = process_arc.handles.lock().allocate_event(obj, EVENT_ALL_ACCESS);
            println!("[OBJMGR] NtCreateEvent -> handle {}", handle);
            handle as u64
        }

        // ----- NtOpenEvent -----
        // arg0 = either a PUNICODE_STRING pointer (user address) or a raw
        //        handle value when the caller passes a pre-created handle.
        //
        // Naming convention for VEILOS simplified ABI:
        //   arg0 >= 0x1000  → treat as a user-space pointer to a
        //                     null-terminated UTF-16LE name string;
        //                     look up in \BaseNamedObjects\.
        //   arg0 < 0x1000   → treat as an existing handle (duplicate / alias).
        //   arg0 == 0       → anonymous; create a new unnamed event.
        //
        // arg1 = desired access mask (EVENT_ALL_ACCESS = 0x1F0003).
        // arg2 = auto_reset flag (used only when creating a new anonymous event).
        0x78 => {
            const EVENT_ALL_ACCESS: u32 = 0x001F_0003;
            let desired_access = if arg1 == 0 { EVENT_ALL_ACCESS } else { arg1 as u32 };

            if arg0 == 0 {
                // Fully anonymous: create a new auto-reset event in the non-signaled state.
                let obj = OBJECT_MANAGER.lock().create_event(None, arg2 != 0, false);
                let handle = process_arc.handles.lock().allocate_event(obj, desired_access);
                println!("[OBJMGR] NtOpenEvent (anon) -> handle {}", handle);
                handle as u64

            } else if arg0 < 0x1000 {
                // Handle-alias path: arg0 is already a valid handle in this
                // process's handle table.  Verify it refers to an event and
                // return a new handle entry pointing to the same Arc.
                let existing = process_arc.handles.lock().as_event(arg0 as Handle);
                match existing {
                    Some(ev) => {
                        let handle = process_arc.handles.lock().allocate_event(ev, desired_access);
                        println!("[OBJMGR] NtOpenEvent alias handle={} -> {}", arg0, handle);
                        handle as u64
                    }
                    None => {
                        println!("[OBJMGR] NtOpenEvent: alias handle {} is not an event", arg0);
                        INVALID_HANDLE_VALUE as u64
                    }
                }

            } else {
                // Named-object path: arg0 is a user-space pointer to a
                // null-terminated ASCII or UTF-16LE string.
                // We read it as ASCII (byte-by-byte, stopping at NUL or NUL-word).
                let name = {
                    let ptr = arg0 as *const u8;
                    let mut s = alloc::string::String::new();
                    for i in 0..256usize {
                        let b = unsafe { *ptr.add(i) };
                        if b == 0 { break; }
                        // Skip the high byte of UTF-16LE pairs (every odd byte = 0x00
                        // for ASCII-range names).
                        if b != 0x00 { s.push(b as char); }
                    }
                    s
                };

                if name.is_empty() {
                    // Pointer exists but name is empty → anonymous.
                    let obj = OBJECT_MANAGER.lock().create_event(None, arg2 != 0, false);
                    let handle = process_arc.handles.lock().allocate_event(obj, desired_access);
                    println!("[OBJMGR] NtOpenEvent (empty name, anon) -> handle {}", handle);
                    return handle as u64;
                }

                println!("[OBJMGR] NtOpenEvent: looking up '{}' in \\BaseNamedObjects\\", name);

                // Check whether an event with this name already exists in any
                // live process's handle table.  We search all processes for
                // a named EventObject matching `name`.
                let existing_obj: Option<alloc::sync::Arc<EventObject>> = {
                    let procs = crate::process::PROCESSOR.lock();
                    procs.processes.iter().find_map(|p| {
                        let table = p.handles.lock();
                        // Scan all handle entries for a matching named event.
                        table.entries.iter().flatten().find_map(|entry| {
                            if let KernelObjectRef::Event(ref ev) = entry.object {
                                if ev.name.as_deref()
                                    .map(|n| n.eq_ignore_ascii_case(&name))
                                    .unwrap_or(false)
                                {
                                    return Some(ev.clone());
                                }
                            }
                            None
                        })
                    })
                };

                match existing_obj {
                    Some(ev) => {
                        // Found — open a new handle to the same Arc.
                        let handle = process_arc.handles.lock().allocate_event(ev, desired_access);
                        println!("[OBJMGR] NtOpenEvent '{}' -> handle {}", name, handle);
                        handle as u64
                    }
                    None => {
                        // Not found — create a new named event so the caller
                        // can proceed (matches Windows behaviour for
                        // CreateEvent with a name that doesn't yet exist).
                        let obj = OBJECT_MANAGER.lock().create_event(
                            Some(&name), arg2 != 0, false,
                        );
                        let handle = process_arc.handles.lock().allocate_event(obj, desired_access);
                        println!("[OBJMGR] NtOpenEvent '{}' not found, created -> handle {}", name, handle);
                        handle as u64
                    }
                }
            }
        }

        // ----- NtSetEvent -----
        0x0E => {
            let handle = arg0 as Handle;
            match process_arc.handles.lock().as_event(handle) {
                Some(ev) => { ev.set(); println!("[OBJMGR] NtSetEvent handle={}", handle); STATUS_SUCCESS }
                None     => { println!("[OBJMGR] NtSetEvent: invalid handle {}", handle); STATUS_INVALID_HANDLE }
            }
        }

        // ----- NtClearEvent / NtResetEvent -----
        0xCE | 0x9D => {
            let handle = arg0 as Handle;
            match process_arc.handles.lock().as_event(handle) {
                Some(ev) => { ev.reset(); println!("[OBJMGR] NtResetEvent handle={}", handle); STATUS_SUCCESS }
                None     => { println!("[OBJMGR] NtResetEvent: invalid handle {}", handle); STATUS_INVALID_HANDLE }
            }
        }

        // ----- NtCreateMutant -----
        0x5C => {
            let initially_owned = arg1 != 0;
            let obj = OBJECT_MANAGER.lock().create_mutex(None, initially_owned);
            let handle = process_arc.handles.lock().allocate_mutex(obj, MUTANT_ALL_ACCESS);
            println!("[OBJMGR] NtCreateMutant -> handle {}", handle);
            handle as u64
        }

        // ----- NtOpenMutant -----
        0x77 => {
            println!("[OBJMGR] NtOpenMutant: user-mode name resolution not yet implemented.");
            INVALID_HANDLE_VALUE as u64
        }

        // ----- NtReleaseMutant -----
        0xAB => {
            let handle = arg0 as Handle;
            match process_arc.handles.lock().as_mutex(handle) {
                Some(m) => {
                    match m.release(pid) {
                        Ok(prev) => {
                            println!("[OBJMGR] NtReleaseMutant handle={} (was recursion={})", handle, prev);
                            STATUS_SUCCESS
                        }
                        Err(e) => {
                            println!("[OBJMGR] NtReleaseMutant FAILED: {}", e);
                            STATUS_MUTANT_NOT_OWNED
                        }
                    }
                }
                None => { println!("[OBJMGR] NtReleaseMutant: invalid handle {}", handle); STATUS_INVALID_HANDLE }
            }
        }

        // ----- NtCreateSemaphore -----
        0x5E => {
            let initial_count = arg1 as i32;
            let max_count     = if arg2 == 0 { i32::MAX } else { arg2 as i32 };
            let obj = OBJECT_MANAGER.lock().create_semaphore(None, initial_count, max_count);
            let handle = process_arc.handles.lock().allocate_semaphore(obj, SEMAPHORE_ALL_ACCESS);
            println!("[OBJMGR] NtCreateSemaphore -> handle {}", handle);
            handle as u64
        }

        // ----- NtOpenSemaphore -----
        0x7A => {
            println!("[OBJMGR] NtOpenSemaphore: user-mode name resolution not yet implemented.");
            INVALID_HANDLE_VALUE as u64
        }

        // ----- NtReleaseSemaphore -----
        0xAC => {
            let handle        = arg0 as Handle;
            let release_count = if arg1 == 0 { 1 } else { arg1 as i32 };
            match process_arc.handles.lock().as_semaphore(handle) {
                Some(sem) => {
                    match sem.release(release_count) {
                        Ok(prev) => {
                            println!("[OBJMGR] NtReleaseSemaphore handle={} released {} (was {})", handle, release_count, prev);
                            STATUS_SUCCESS
                        }
                        Err(e) => {
                            println!("[OBJMGR] NtReleaseSemaphore FAILED: {}", e);
                            STATUS_SEMAPHORE_LIMIT
                        }
                    }
                }
                None => { println!("[OBJMGR] NtReleaseSemaphore: invalid handle {}", handle); STATUS_INVALID_HANDLE }
            }
        }

        // ----- NtCreateSection -----
        0x4A => {
            let size       = arg1 as usize;
            let protection = if arg2 == 0 { PAGE_READWRITE } else { arg2 as u32 };
            if size == 0 {
                println!("[OBJMGR] NtCreateSection: size cannot be zero");
                return STATUS_INVALID_PARAMETER;
            }
            let obj = OBJECT_MANAGER.lock().create_section(None, size, protection);
            let handle = process_arc.handles.lock().allocate_section(obj, SECTION_ALL_ACCESS);
            println!("[OBJMGR] NtCreateSection -> handle {} ({} bytes)", handle, size);
            handle as u64
        }

        // ----- NtOpenSection -----
        0x79 => {
            println!("[OBJMGR] NtOpenSection: user-mode name resolution not yet implemented.");
            INVALID_HANDLE_VALUE as u64
        }

        // ----- NtClose -----
        0x0F => {
            let handle = arg0 as Handle;
            let closed = process_arc.handles.lock().close(handle);
            if closed {
                println!("[OBJMGR] NtClose: handle {} freed", handle);
                STATUS_SUCCESS
            } else {
                println!("[OBJMGR] NtClose: invalid handle {}", handle);
                STATUS_INVALID_HANDLE
            }
        }

        // ----- NtWaitForSingleObject -----
        // We implement a non-blocking "try-once" check. True blocking waits
        // require integrating with the Task executor (future work).
        0x04 => {
            let handle   = arg0 as Handle;
            let _timeout = arg1;

            let obj_type = match process_arc.handles.lock().get_type(handle) {
                Some(t) => t,
                None => {
                    println!("[OBJMGR] NtWaitForSingleObject: invalid handle {}", handle);
                    return STATUS_INVALID_HANDLE;
                }
            };

            match obj_type {
                ObjectType::Event => {
                    if let Some(ev) = process_arc.handles.lock().as_event(handle) {
                        if ev.test_and_consume() {
                            println!("[OBJMGR] NtWait(Event): signaled — returning SUCCESS");
                            return STATUS_SUCCESS;
                        }
                    }
                }
                ObjectType::Mutex => {
                    if let Some(m) = process_arc.handles.lock().as_mutex(handle) {
                        if m.try_acquire(pid) {
                            println!("[OBJMGR] NtWait(Mutex): acquired — returning SUCCESS");
                            return STATUS_SUCCESS;
                        }
                    }
                }
                ObjectType::Semaphore => {
                    if let Some(s) = process_arc.handles.lock().as_semaphore(handle) {
                        if s.try_acquire() {
                            println!("[OBJMGR] NtWait(Semaphore): acquired — returning SUCCESS");
                            return STATUS_SUCCESS;
                        }
                    }
                }
                t => {
                    println!("[OBJMGR] NtWaitForSingleObject: unsupported object type {:?}", t);
                    return STATUS_NOT_IMPLEMENTED;
                }
            }

            println!("[OBJMGR] NtWaitForSingleObject handle={}: not ready — STATUS_TIMEOUT", handle);
            STATUS_TIMEOUT
        }

        _ => STATUS_NOT_IMPLEMENTED,
    }
}
