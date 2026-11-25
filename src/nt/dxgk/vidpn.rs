// VidPN: the video present network objects dxgkrnl owns and display
// drivers fill in, through DXGK_VIDPN_INTERFACE and its topology and mode
// set interfaces (DxgkCbQueryVidPnInterface), plus the monitor's source
// modes (DxgkCbQueryMonitorInterface).
//
// A VidPN is a topology (present paths: source -> target, with their
// scaling, rotation and so on) and, per source and per target, a set of
// modes, one of which may be pinned. dxgkrnl builds one (`commit`), asks the
// driver whether it can do it (DxgkDdiIsSupportedVidPn), lets it fill the
// mode sets it can do with what's there (DxgkDdiEnumVidPnCofuncModality),
// pins the modes it wants, and commits (DxgkDdiCommitVidPn).
//
// Objects live here, in Rust; what the driver reads and writes through
// pointers (path infos, mode infos) is pool memory in the WDK's layouts
// (checked in k1's layout.c). Handles are numbers.

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};
use spin::Mutex;

use super::{ddi, rd, with_adapter, wr, Display, STATUS_NOT_SUPPORTED};
use crate::nt::ntos::{ke, pool, NtStatus, STATUS_INVALID_PARAMETER, STATUS_NO_MEMORY, STATUS_SUCCESS};

const STATUS_GRAPHICS_INVALID_VIDPN: NtStatus = 0xC01E_0303u32 as i32;
const STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY: NtStatus = 0xC01E_0300u32 as i32;
const STATUS_GRAPHICS_PATH_ALREADY_IN_TOPOLOGY: NtStatus = 0xC01E_0313u32 as i32;
const STATUS_GRAPHICS_PATH_NOT_IN_TOPOLOGY: NtStatus = 0xC01E_0327u32 as i32;
const STATUS_GRAPHICS_MODE_ALREADY_IN_MODESET: NtStatus = 0xC01E_0314u32 as i32;
const STATUS_GRAPHICS_MODE_NOT_IN_MODESET: NtStatus = 0xC01E_0329u32 as i32;
const STATUS_GRAPHICS_INVALID_MODE_SET: NtStatus = 0xC01E_0309u32 as i32;
const STATUS_GRAPHICS_DATASET_IS_EMPTY: NtStatus = 0x401E_034B;
const STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET: NtStatus = 0x401E_034C;
const STATUS_GRAPHICS_SOURCE_NOT_IN_TOPOLOGY: NtStatus = 0xC01E_0321u32 as i32;
const STATUS_GRAPHICS_TARGET_NOT_IN_TOPOLOGY: NtStatus = 0xC01E_0322u32 as i32;

// D3DKMDT_VIDPN_PRESENT_PATH (0x168).
const PATH_SIZE: usize = 0x168;
const PA_TARGET: usize = 0x04;
const PA_IMPORTANCE: usize = 0x08;
const PA_SCALING: usize = 0x0C;
const PA_SCALING_SUPPORT: usize = 0x10;
const PA_ROTATION: usize = 0x14;
const PA_ROTATION_SUPPORT: usize = 0x18;
const PA_COLOR_BASIS: usize = 0x2C;
const PA_CONTENT: usize = 0x40;
const PA_COPY_PROTECTION: usize = 0x44;
const PA_GAMMA_RAMP: usize = 0x150;
// Mode infos: D3DKMDT_VIDPN_SOURCE_MODE (0x28), D3DKMDT_VIDPN_TARGET_MODE
// (0x50 with WDDM 2.9's MinimumVSyncFreq; earlier drivers use less),
// D3DKMDT_MONITOR_SOURCE_MODE (0x60). Id first in each.
const SOURCE_MODE_SIZE: usize = 0x28;
const TARGET_MODE_SIZE: usize = 0x50;
const MONITOR_MODE_SIZE: usize = 0x60;
const SM_TYPE: usize = 0x04;
const SM_SIZE: usize = 0x08;
const SM_STRIDE: usize = 0x18;
const SM_FORMAT: usize = 0x1C;
// D3DKMDT_VIDEO_SIGNAL_INFO, at 0x08 in target and monitor modes.
const VSI: usize = 0x08;
const VSI_TOTAL: usize = 0x04;
const VSI_ACTIVE: usize = 0x0C;
const VSI_VSYNC: usize = 0x14;
const VSI_HSYNC: usize = 0x1C;
const VSI_PIXEL_RATE: usize = 0x28;
const VSI_SCANLINE: usize = 0x30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Source,
    Target,
    Monitor,
}

impl Kind {
    fn size(self) -> usize {
        match self {
            Kind::Source => SOURCE_MODE_SIZE,
            Kind::Target => TARGET_MODE_SIZE,
            Kind::Monitor => MONITOR_MODE_SIZE,
        }
    }
}

struct ModeSet {
    kind: Kind,
    modes: Vec<usize>,
    /// Infos made by CreateNewModeInfo and not (yet) added.
    loose: Vec<usize>,
    pinned: Option<u32>,
    preferred: Option<u32>,
    next_id: u32,
}

struct VidPn {
    paths: Vec<[u8; PATH_SIZE]>,
    sources: BTreeMap<u32, u64>,
    targets: BTreeMap<u32, u64>,
}

struct Objects {
    next: u64,
    vidpns: BTreeMap<u64, VidPn>,
    sets: BTreeMap<u64, ModeSet>,
    /// Path infos handed to the driver (copies): pointer -> (source, target).
    path_copies: BTreeMap<usize, (u32, u32)>,
    /// Monitor source mode sets, by (adapter, target).
    monitors: BTreeMap<(usize, u32), u64>,
}

static OBJ: Mutex<Objects> = Mutex::new(Objects {
    next: 0x1000,
    vidpns: BTreeMap::new(),
    sets: BTreeMap::new(),
    path_copies: BTreeMap::new(),
    monitors: BTreeMap::new(),
});

const TOPOLOGY: u64 = 1 << 40;

fn locked<R>(f: impl FnOnce(&mut Objects) -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(|| f(&mut OBJ.lock()))
}

fn new_handle(o: &mut Objects) -> u64 {
    o.next += 1;
    o.next
}

fn new_set(o: &mut Objects, kind: Kind) -> u64 {
    let h = new_handle(o);
    o.sets.insert(h, ModeSet { kind, modes: Vec::new(), loose: Vec::new(), pinned: None, preferred: None, next_id: 1 });
    h
}

fn free_set(o: &mut Objects, h: u64) {
    if let Some(s) = o.sets.remove(&h) {
        for p in s.modes.into_iter().chain(s.loose) {
            pool::free_tracked(p as *mut u8);
        }
    }
}

fn mode_id(p: usize) -> u32 {
    unsafe { rd::<u32>(p as *const u8, 0) }
}

// -----| interface tables (built once, in pool) |-----

static TABLES: Mutex<[usize; 6]> = Mutex::new([0; 6]);
const T_VIDPN: usize = 0;
const T_TOPOLOGY: usize = 1;
const T_SOURCE_SET: usize = 2;
const T_TARGET_SET: usize = 3;
const T_MONITOR: usize = 4;
const T_MONITOR_SET: usize = 5;

fn table(which: usize) -> u64 {
    let mut t = TABLES.lock();
    if t[which] != 0 {
        return t[which] as u64;
    }
    let fns: Vec<*const ()> = match which {
        T_VIDPN => alloc::vec![
            get_topology as *const (),
            acquire_source_mode_set as *const (),
            release_mode_set_of_vidpn as *const (),
            create_new_source_mode_set as *const (),
            assign_source_mode_set as *const (),
            assign_multisampling_method_set as *const (),
            acquire_target_mode_set as *const (),
            release_mode_set_of_vidpn as *const (),
            create_new_target_mode_set as *const (),
            assign_target_mode_set as *const (),
        ],
        T_TOPOLOGY => alloc::vec![
            get_num_paths as *const (),
            get_num_paths_from_source as *const (),
            enum_path_targets_from_source as *const (),
            get_path_source_from_target as *const (),
            acquire_path_info as *const (),
            acquire_first_path_info as *const (),
            acquire_next_path_info as *const (),
            update_path_support_info as *const (),
            release_path_info as *const (),
            create_new_path_info as *const (),
            add_path as *const (),
            remove_path as *const (),
        ],
        T_SOURCE_SET | T_TARGET_SET => alloc::vec![
            set_get_num_modes as *const (),
            set_acquire_first as *const (),
            set_acquire_next as *const (),
            set_acquire_pinned as *const (),
            set_release_mode as *const (),
            set_create_new as *const (),
            set_add_mode as *const (),
            set_pin_mode as *const (),
        ],
        T_MONITOR => alloc::vec![
            acquire_monitor_source_mode_set as *const (),
            release_monitor_source_mode_set as *const (),
            monitor_not_supported as *const (),
            monitor_not_supported as *const (),
        ],
        _ => alloc::vec![
            set_get_num_modes as *const (),
            set_acquire_preferred as *const (),
            set_acquire_first as *const (),
            set_acquire_next as *const (),
            set_create_new as *const (),
            set_add_mode as *const (),
            set_release_mode as *const (),
        ],
    };
    // DXGK_VIDPN_INTERFACE and DXGK_MONITOR_INTERFACE start with Version;
    // the others are function pointers only.
    let versioned = which == T_VIDPN || which == T_MONITOR;
    let n = fns.len() + versioned as usize;
    let p = pool::alloc_tracked(n * 8);
    unsafe {
        let mut at = 0;
        if versioned {
            wr::<u64>(p, 0, 1);
            at = 8;
        }
        for (i, f) in fns.iter().enumerate() {
            wr::<u64>(p, at + i * 8, *f as u64);
        }
    }
    t[which] = p as usize;
    p as u64
}

/// DxgkCbQueryVidPnInterface(hVidPn, Version, ppVidPnInterface).
pub extern "win64" fn query_vidpn_interface(h: u64, _version: u32, out: *mut u64) -> NtStatus {
    if !locked(|o| o.vidpns.contains_key(&h)) {
        return STATUS_GRAPHICS_INVALID_VIDPN;
    }
    unsafe { out.write(table(T_VIDPN)) };
    STATUS_SUCCESS
}

/// DxgkCbQueryMonitorInterface(DeviceHandle, Version, ppMonitorInterface).
pub extern "win64" fn query_monitor_interface(_h: u64, _version: u32, out: *mut u64) -> NtStatus {
    unsafe { out.write(table(T_MONITOR)) };
    STATUS_SUCCESS
}

// -----| DXGK_VIDPN_INTERFACE |-----

extern "win64" fn get_topology(h: u64, topo: *mut u64, iface: *mut u64) -> NtStatus {
    if !locked(|o| o.vidpns.contains_key(&h)) {
        return STATUS_GRAPHICS_INVALID_VIDPN;
    }
    unsafe {
        topo.write(h | TOPOLOGY);
        iface.write(table(T_TOPOLOGY));
    }
    STATUS_SUCCESS
}

fn acquire_set(h: u64, id: u32, target: bool, out: *mut u64, iface: *mut u64) -> NtStatus {
    let r = locked(|o| {
        let existing = {
            let v = o.vidpns.get(&h)?;
            if target { v.targets.get(&id).copied() } else { v.sources.get(&id).copied() }
        };
        Some(match existing {
            Some(s) => s,
            None => {
                let s = new_set(o, if target { Kind::Target } else { Kind::Source });
                let v = o.vidpns.get_mut(&h)?;
                if target {
                    v.targets.insert(id, s);
                } else {
                    v.sources.insert(id, s);
                }
                s
            }
        })
    });
    let Some(s) = r else { return STATUS_GRAPHICS_INVALID_VIDPN };
    unsafe {
        out.write(s);
        iface.write(table(if target { T_TARGET_SET } else { T_SOURCE_SET }));
    }
    STATUS_SUCCESS
}

extern "win64" fn acquire_source_mode_set(h: u64, src: u32, out: *mut u64, iface: *mut u64) -> NtStatus {
    acquire_set(h, src, false, out, iface)
}

extern "win64" fn acquire_target_mode_set(h: u64, tgt: u32, out: *mut u64, iface: *mut u64) -> NtStatus {
    acquire_set(h, tgt, true, out, iface)
}

/// Releasing a set the VidPN holds changes nothing; one made with
/// CreateNew*ModeSet and never assigned is freed.
extern "win64" fn release_mode_set_of_vidpn(h: u64, set: u64) -> NtStatus {
    locked(|o| {
        let held = o.vidpns.get(&h).is_some_and(|v| v.sources.values().chain(v.targets.values()).any(|&s| s == set));
        if !held {
            free_set(o, set);
        }
    });
    STATUS_SUCCESS
}

fn create_set(h: u64, target: bool, out: *mut u64, iface: *mut u64) -> NtStatus {
    let r = locked(|o| {
        o.vidpns.get(&h)?;
        Some(new_set(o, if target { Kind::Target } else { Kind::Source }))
    });
    let Some(s) = r else { return STATUS_GRAPHICS_INVALID_VIDPN };
    unsafe {
        out.write(s);
        iface.write(table(if target { T_TARGET_SET } else { T_SOURCE_SET }));
    }
    STATUS_SUCCESS
}

extern "win64" fn create_new_source_mode_set(h: u64, _src: u32, out: *mut u64, iface: *mut u64) -> NtStatus {
    create_set(h, false, out, iface)
}

extern "win64" fn create_new_target_mode_set(h: u64, _tgt: u32, out: *mut u64, iface: *mut u64) -> NtStatus {
    create_set(h, true, out, iface)
}

fn assign_set(h: u64, id: u32, set: u64, target: bool) -> NtStatus {
    locked(|o| {
        if !o.sets.contains_key(&set) {
            return STATUS_GRAPHICS_INVALID_MODE_SET;
        }
        let old = {
            let Some(v) = o.vidpns.get_mut(&h) else { return STATUS_GRAPHICS_INVALID_VIDPN };
            if target { v.targets.insert(id, set) } else { v.sources.insert(id, set) }
        };
        if let Some(old) = old.filter(|&s| s != set) {
            free_set(o, old);
        }
        STATUS_SUCCESS
    })
}

extern "win64" fn assign_source_mode_set(h: u64, src: u32, set: u64) -> NtStatus {
    assign_set(h, src, set, false)
}

extern "win64" fn assign_target_mode_set(h: u64, tgt: u32, set: u64) -> NtStatus {
    assign_set(h, tgt, set, true)
}

extern "win64" fn assign_multisampling_method_set(_h: u64, _src: u32, _n: u32, _methods: *const u8) -> NtStatus {
    STATUS_SUCCESS
}

// -----| DXGK_VIDPNTOPOLOGY_INTERFACE |-----

fn vidpn_of(topo: u64) -> u64 {
    topo & !TOPOLOGY
}

fn path_src(p: &[u8]) -> u32 {
    u32::from_le_bytes(p[0..4].try_into().unwrap())
}

fn path_tgt(p: &[u8]) -> u32 {
    u32::from_le_bytes(p[PA_TARGET..PA_TARGET + 4].try_into().unwrap())
}

/// Counts and indexes are SIZE_T.
extern "win64" fn get_num_paths(t: u64, n: *mut u64) -> NtStatus {
    match locked(|o| o.vidpns.get(&vidpn_of(t)).map(|v| v.paths.len())) {
        Some(c) => {
            unsafe { n.write(c as u64) };
            STATUS_SUCCESS
        }
        None => STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY,
    }
}

extern "win64" fn get_num_paths_from_source(t: u64, src: u32, n: *mut u64) -> NtStatus {
    match locked(|o| o.vidpns.get(&vidpn_of(t)).map(|v| v.paths.iter().filter(|p| path_src(&p[..]) == src).count())) {
        Some(0) => STATUS_GRAPHICS_SOURCE_NOT_IN_TOPOLOGY,
        Some(c) => {
            unsafe { n.write(c as u64) };
            STATUS_SUCCESS
        }
        None => STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY,
    }
}

extern "win64" fn enum_path_targets_from_source(t: u64, src: u32, index: u64, tgt: *mut u32) -> NtStatus {
    let r = locked(|o| {
        o.vidpns.get(&vidpn_of(t)).map(|v| v.paths.iter().filter(|p| path_src(&p[..]) == src).nth(index as usize).map(|p| path_tgt(&p[..])))
    });
    match r {
        Some(Some(x)) => {
            unsafe { tgt.write(x) };
            STATUS_SUCCESS
        }
        Some(None) => STATUS_GRAPHICS_SOURCE_NOT_IN_TOPOLOGY,
        None => STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY,
    }
}

extern "win64" fn get_path_source_from_target(t: u64, tgt: u32, src: *mut u32) -> NtStatus {
    let r = locked(|o| o.vidpns.get(&vidpn_of(t)).map(|v| v.paths.iter().find(|p| path_tgt(&p[..]) == tgt).map(|p| path_src(&p[..]))));
    match r {
        Some(Some(x)) => {
            unsafe { src.write(x) };
            STATUS_SUCCESS
        }
        Some(None) => STATUS_GRAPHICS_TARGET_NOT_IN_TOPOLOGY,
        None => STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY,
    }
}

/// A pool copy of path `index` of the topology, recorded so Update and
/// Release find it.
fn copy_path(t: u64, index: usize, out: *mut u64) -> NtStatus {
    let r = locked(|o| {
        let p = o.vidpns.get(&vidpn_of(t))?.paths.get(index).copied();
        Some(p)
    });
    let p = match r {
        None => return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY,
        Some(None) => {
            unsafe { out.write(0) };
            return STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET;
        }
        Some(Some(p)) => p,
    };
    let c = pool::alloc_tracked(PATH_SIZE);
    if c.is_null() {
        return STATUS_NO_MEMORY;
    }
    unsafe { core::ptr::copy_nonoverlapping(p.as_ptr(), c, PATH_SIZE) };
    locked(|o| o.path_copies.insert(c as usize, (path_src(&p[..]), path_tgt(&p[..]))));
    unsafe { out.write(c as u64) };
    STATUS_SUCCESS
}

fn path_index(t: u64, src: u32, tgt: u32) -> Option<usize> {
    locked(|o| o.vidpns.get(&vidpn_of(t)).and_then(|v| v.paths.iter().position(|p| path_src(&p[..]) == src && path_tgt(&p[..]) == tgt)))
}

extern "win64" fn acquire_path_info(t: u64, src: u32, tgt: u32, out: *mut u64) -> NtStatus {
    match path_index(t, src, tgt) {
        Some(i) => copy_path(t, i, out),
        None => STATUS_GRAPHICS_PATH_NOT_IN_TOPOLOGY,
    }
}

extern "win64" fn acquire_first_path_info(t: u64, out: *mut u64) -> NtStatus {
    let st = copy_path(t, 0, out);
    if st == STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET { STATUS_GRAPHICS_DATASET_IS_EMPTY } else { st }
}

extern "win64" fn acquire_next_path_info(t: u64, current: *const u8, out: *mut u64) -> NtStatus {
    let (src, tgt) = unsafe { (rd::<u32>(current, 0), rd::<u32>(current, PA_TARGET)) };
    match path_index(t, src, tgt) {
        Some(i) => copy_path(t, i + 1, out),
        None => STATUS_GRAPHICS_PATH_NOT_IN_TOPOLOGY,
    }
}

/// The driver changed what a path supports (scaling, rotation): copy its
/// info back into the topology.
extern "win64" fn update_path_support_info(t: u64, info: *const u8) -> NtStatus {
    let mut p = [0u8; PATH_SIZE];
    unsafe { core::ptr::copy_nonoverlapping(info, p.as_mut_ptr(), PATH_SIZE) };
    let (src, tgt) = (path_src(&p[..]), path_tgt(&p[..]));
    locked(|o| {
        let Some(v) = o.vidpns.get_mut(&vidpn_of(t)) else { return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY };
        match v.paths.iter_mut().find(|x| path_src(&x[..]) == src && path_tgt(&x[..]) == tgt) {
            Some(x) => {
                *x = p;
                STATUS_SUCCESS
            }
            None => STATUS_GRAPHICS_PATH_NOT_IN_TOPOLOGY,
        }
    })
}

extern "win64" fn release_path_info(_t: u64, info: *mut u8) -> NtStatus {
    if locked(|o| o.path_copies.remove(&(info as usize))).is_some() {
        pool::free_tracked(info);
    }
    STATUS_SUCCESS
}

/// A new path with Windows' defaults: primary, identity scaling and
/// rotation, sRGB, graphics content, no copy protection, default gamma.
fn default_path(src: u32, tgt: u32) -> [u8; PATH_SIZE] {
    let mut p = [0u8; PATH_SIZE];
    let mut put = |off: usize, v: u32| p[off..off + 4].copy_from_slice(&v.to_le_bytes());
    put(0, src);
    put(PA_TARGET, tgt);
    put(PA_IMPORTANCE, 1);
    put(PA_SCALING, 1);
    put(PA_SCALING_SUPPORT, 1);
    put(PA_ROTATION, 1);
    put(PA_ROTATION_SUPPORT, 1);
    put(PA_COLOR_BASIS, 2);
    put(PA_CONTENT, 1);
    put(PA_COPY_PROTECTION, 1);
    put(PA_GAMMA_RAMP, 1);
    p
}

extern "win64" fn create_new_path_info(_t: u64, out: *mut u64) -> NtStatus {
    let c = pool::alloc_tracked(PATH_SIZE);
    if c.is_null() {
        return STATUS_NO_MEMORY;
    }
    let p = default_path(0, 0);
    unsafe {
        core::ptr::copy_nonoverlapping(p.as_ptr(), c, PATH_SIZE);
        out.write(c as u64);
    }
    STATUS_SUCCESS
}

/// AddPath takes the info (made by CreateNewPathInfo) into the topology.
extern "win64" fn add_path(t: u64, info: *mut u8) -> NtStatus {
    let mut p = [0u8; PATH_SIZE];
    unsafe { core::ptr::copy_nonoverlapping(info, p.as_mut_ptr(), PATH_SIZE) };
    let st = locked(|o| {
        let Some(v) = o.vidpns.get_mut(&vidpn_of(t)) else { return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY };
        if v.paths.iter().any(|x| path_tgt(&x[..]) == path_tgt(&p[..])) {
            return STATUS_GRAPHICS_PATH_ALREADY_IN_TOPOLOGY;
        }
        v.paths.push(p);
        STATUS_SUCCESS
    });
    if st >= 0 {
        pool::free_tracked(info);
    }
    st
}

extern "win64" fn remove_path(t: u64, src: u32, tgt: u32) -> NtStatus {
    locked(|o| {
        let Some(v) = o.vidpns.get_mut(&vidpn_of(t)) else { return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY };
        let before = v.paths.len();
        v.paths.retain(|x| !(path_src(&x[..]) == src && path_tgt(&x[..]) == tgt));
        if v.paths.len() == before { STATUS_GRAPHICS_PATH_NOT_IN_TOPOLOGY } else { STATUS_SUCCESS }
    })
}

// -----| mode sets: source, target and monitor |-----

extern "win64" fn set_get_num_modes(s: u64, n: *mut u64) -> NtStatus {
    match locked(|o| o.sets.get(&s).map(|x| x.modes.len())) {
        Some(c) => {
            unsafe { n.write(c as u64) };
            STATUS_SUCCESS
        }
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

extern "win64" fn set_acquire_first(s: u64, out: *mut u64) -> NtStatus {
    match locked(|o| o.sets.get(&s).map(|x| x.modes.first().copied())) {
        Some(Some(p)) => {
            unsafe { out.write(p as u64) };
            STATUS_SUCCESS
        }
        Some(None) => {
            unsafe { out.write(0) };
            STATUS_GRAPHICS_DATASET_IS_EMPTY
        }
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

extern "win64" fn set_acquire_next(s: u64, current: *const u8, out: *mut u64) -> NtStatus {
    let r = locked(|o| {
        o.sets.get(&s).map(|x| {
            let at = x.modes.iter().position(|&p| p == current as usize);
            at.and_then(|i| x.modes.get(i + 1).copied())
        })
    });
    match r {
        Some(Some(p)) => {
            unsafe { out.write(p as u64) };
            STATUS_SUCCESS
        }
        Some(None) => {
            unsafe { out.write(0) };
            STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET
        }
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

/// The pinned mode, or success with null when none is pinned.
extern "win64" fn set_acquire_pinned(s: u64, out: *mut u64) -> NtStatus {
    let r = locked(|o| o.sets.get(&s).map(|x| x.pinned.and_then(|id| x.modes.iter().copied().find(|&p| mode_id(p) == id))));
    match r {
        Some(p) => {
            unsafe { out.write(p.unwrap_or(0) as u64) };
            STATUS_SUCCESS
        }
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

extern "win64" fn set_acquire_preferred(s: u64, out: *mut u64) -> NtStatus {
    let r = locked(|o| o.sets.get(&s).map(|x| x.preferred.and_then(|id| x.modes.iter().copied().find(|&p| mode_id(p) == id))));
    match r {
        Some(Some(p)) => {
            unsafe { out.write(p as u64) };
            STATUS_SUCCESS
        }
        Some(None) => {
            unsafe { out.write(0) };
            STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET
        }
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

/// Infos in a set stay the set's; a new one never added is freed.
extern "win64" fn set_release_mode(s: u64, info: *mut u8) -> NtStatus {
    let free = locked(|o| {
        let Some(x) = o.sets.get_mut(&s) else { return false };
        let before = x.loose.len();
        x.loose.retain(|&p| p != info as usize);
        before != x.loose.len()
    });
    if free {
        pool::free_tracked(info);
    }
    STATUS_SUCCESS
}

extern "win64" fn set_create_new(s: u64, out: *mut u64) -> NtStatus {
    let r = locked(|o| {
        let x = o.sets.get_mut(&s)?;
        let p = pool::alloc_tracked(x.kind.size());
        if p.is_null() {
            return Some(None);
        }
        unsafe { wr::<u32>(p, 0, x.next_id) };
        x.next_id += 1;
        x.loose.push(p as usize);
        Some(Some(p as usize))
    });
    match r {
        Some(Some(p)) => {
            unsafe { out.write(p as u64) };
            STATUS_SUCCESS
        }
        Some(None) => STATUS_NO_MEMORY,
        None => STATUS_GRAPHICS_INVALID_MODE_SET,
    }
}

extern "win64" fn set_add_mode(s: u64, info: *mut u8) -> NtStatus {
    locked(|o| {
        let Some(x) = o.sets.get_mut(&s) else { return STATUS_GRAPHICS_INVALID_MODE_SET };
        if !x.loose.contains(&(info as usize)) {
            return STATUS_INVALID_PARAMETER;
        }
        let id = mode_id(info as usize);
        if x.modes.iter().any(|&p| mode_id(p) == id) {
            return STATUS_GRAPHICS_MODE_ALREADY_IN_MODESET;
        }
        x.loose.retain(|&p| p != info as usize);
        x.modes.push(info as usize);
        STATUS_SUCCESS
    })
}

extern "win64" fn set_pin_mode(s: u64, id: u32) -> NtStatus {
    locked(|o| {
        let Some(x) = o.sets.get_mut(&s) else { return STATUS_GRAPHICS_INVALID_MODE_SET };
        if !x.modes.iter().any(|&p| mode_id(p) == id) {
            return STATUS_GRAPHICS_MODE_NOT_IN_MODESET;
        }
        x.pinned = Some(id);
        STATUS_SUCCESS
    })
}

// -----| DXGK_MONITOR_INTERFACE |-----

extern "win64" fn acquire_monitor_source_mode_set(h: u64, tgt: u32, out: *mut u64, iface: *mut u64) -> NtStatus {
    let s = locked(|o| o.monitors.get(&(h as usize, tgt)).copied());
    let Some(s) = s else { return STATUS_INVALID_PARAMETER };
    unsafe {
        out.write(s);
        iface.write(table(T_MONITOR_SET));
    }
    STATUS_SUCCESS
}

extern "win64" fn release_monitor_source_mode_set(_h: u64, _s: u64) -> NtStatus {
    STATUS_SUCCESS
}

extern "win64" fn monitor_not_supported(_h: u64) -> NtStatus {
    STATUS_NOT_SUPPORTED
}

// -----| what dxgkrnl does with them |-----

/// Fill a video signal (in a target or monitor mode) for `w` x `h` at 60 Hz,
/// with CVT-like blanking.
fn put_signal(p: *mut u8, w: u32, h: u32) {
    let (tw, th) = (w + w / 4, h + h / 20 + 3);
    unsafe {
        wr::<u32>(p, VSI, 255); // D3DKMDT_VSS_OTHER
        wr::<u32>(p, VSI + VSI_TOTAL, tw);
        wr::<u32>(p, VSI + VSI_TOTAL + 4, th);
        wr::<u32>(p, VSI + VSI_ACTIVE, w);
        wr::<u32>(p, VSI + VSI_ACTIVE + 4, h);
        wr::<u32>(p, VSI + VSI_VSYNC, 60);
        wr::<u32>(p, VSI + VSI_VSYNC + 4, 1);
        wr::<u32>(p, VSI + VSI_HSYNC, th * 60);
        wr::<u32>(p, VSI + VSI_HSYNC + 4, 1);
        wr::<u64>(p, VSI + VSI_PIXEL_RATE, tw as u64 * th as u64 * 60);
        wr::<u32>(p, VSI + VSI_SCANLINE, 1); // progressive
    }
}

/// The monitor on `tgt`: its source modes, the preferred one first, and
/// what the driver recommends (DxgkDdiRecommendMonitorModes).
fn make_monitor_modes(id: usize, tgt: u32, modes: &[(u32, u32)]) {
    let s = locked(|o| {
        if let Some(old) = o.monitors.remove(&(id, tgt)) {
            free_set(o, old);
        }
        let s = new_set(o, Kind::Monitor);
        o.monitors.insert((id, tgt), s);
        s
    });
    for (i, &(w, h)) in modes.iter().enumerate() {
        let mut p: u64 = 0;
        if set_create_new(s, &mut p) < 0 {
            continue;
        }
        let p = p as *mut u8;
        put_signal(p, w, h);
        unsafe {
            wr::<u32>(p, 0x40, 2); // sRGB
            for c in 0..3 {
                wr::<u32>(p, 0x44 + 4 * c, 8);
            }
            wr::<u32>(p, 0x54, if i == 0 { 2 } else { 1 }); // monitor descriptor / default profile
            wr::<u32>(p, 0x58, if i == 0 { 1 } else { 2 }); // preferred / not preferred
        }
        let id_ = mode_id(p as usize);
        set_add_mode(s, p);
        if i == 0 {
            locked(|o| {
                if let Some(x) = o.sets.get_mut(&s) {
                    x.preferred = Some(id_);
                }
            });
        }
    }
    let (f, ctx) = ddi(id, super::DDI_RECOMMEND_MONITOR_MODES);
    if f != 0 {
        // DXGKARG_RECOMMENDMONITORMODES: VideoPresentTargetId 0,
        // hMonitorSourceModeSet 8, pMonitorSourceModeSetInterface 0x10.
        let mut a = [0u8; 0x18];
        unsafe {
            wr::<u32>(a.as_mut_ptr(), 0, tgt);
            wr::<u64>(a.as_mut_ptr(), 8, s);
            wr::<u64>(a.as_mut_ptr(), 0x10, table(T_MONITOR_SET));
        }
        let f: extern "win64" fn(u64, *const u8) -> NtStatus = unsafe { core::mem::transmute(f) };
        let st = f(ctx, a.as_ptr());
        ke::set_irql(0);
        let n = locked(|o| o.sets.get(&s).map_or(0, |x| x.modes.len()));
        super::note(id, format!("DxgkDdiRecommendMonitorModes -> {:#010x}: {} monitor mode(s)", st as u32, n));
    }
}

/// DxgkDdiQueryDeviceDescriptor: the monitor's EDID, and the preferred
/// mode in it (its first detailed timing).
pub fn query_edid(id: usize, tgt: u32) -> Option<(u32, u32)> {
    let (f, ctx) = ddi(id, super::DDI_QUERY_DEVICE_DESCRIPTOR);
    if f == 0 {
        return None;
    }
    let mut edid = [0u8; 128];
    // DXGK_DEVICE_DESCRIPTOR: DescriptorOffset 0, DescriptorLength 4,
    // DescriptorBuffer 8.
    let mut d = [0u8; 0x10];
    unsafe {
        wr::<u32>(d.as_mut_ptr(), 4, 128);
        wr::<u64>(d.as_mut_ptr(), 8, edid.as_mut_ptr() as u64);
    }
    let f: extern "win64" fn(u64, u32, *mut u8) -> NtStatus = unsafe { core::mem::transmute(f) };
    let st = f(ctx, tgt, d.as_mut_ptr());
    ke::set_irql(0);
    if st < 0 {
        super::note(id, format!("DxgkDdiQueryDeviceDescriptor -> {:#010x} (no EDID)", st as u32));
        return None;
    }
    let header_ok = edid[0..8] == [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];
    let sum_ok = edid.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0;
    let t = &edid[54..72];
    let (w, h) = ((t[2] as u32) | ((t[4] as u32 & 0xF0) << 4), (t[5] as u32) | ((t[7] as u32 & 0xF0) << 4));
    let name = edid_name(&edid);
    super::note(id, format!("EDID: header {}, checksum {}, monitor \"{}\", preferred {}x{}", if header_ok { "ok" } else { "bad" },
        if sum_ok { "ok" } else { "bad" }, name, w, h));
    (header_ok && sum_ok && w > 0 && h > 0).then_some((w, h))
}

/// The monitor name descriptor (0xFC) of an EDID.
fn edid_name(e: &[u8; 128]) -> String {
    for d in 0..4 {
        let b = &e[54 + d * 18..72 + d * 18];
        if b[0] == 0 && b[1] == 0 && b[3] == 0xFC {
            return String::from_utf8_lossy(&b[5..18]).trim_end_matches(['\n', ' ']).into();
        }
    }
    String::new()
}

/// Build a VidPN with one path (source 0 -> `tgt`), let the driver fill its
/// mode sets, pin `w` x `h` and commit it.
pub fn commit(id: usize, tgt: u32, w: u32, h: u32) -> Result<(), String> {
    make_monitor_modes(id, tgt, &[(w, h)]);
    let v = locked(|o| {
        let h = new_handle(o);
        o.vidpns.insert(h, VidPn { paths: alloc::vec![default_path(0, tgt)], sources: BTreeMap::new(), targets: BTreeMap::new() });
        h
    });
    let r = commit_in(id, v, tgt, w, h);
    locked(|o| {
        if let Some(x) = o.vidpns.remove(&v) {
            for s in x.sources.values().chain(x.targets.values()) {
                free_set(o, *s);
            }
        }
    });
    r
}

fn call2(id: usize, off: usize, args: &mut [u8]) -> Result<NtStatus, String> {
    let (f, ctx) = ddi(id, off);
    if f == 0 {
        return Err(format!("the driver has no DDI at {:#x}", off));
    }
    let f: extern "win64" fn(u64, *mut u8) -> NtStatus = unsafe { core::mem::transmute(f) };
    let st = f(ctx, args.as_mut_ptr());
    ke::set_irql(0);
    Ok(st)
}

fn is_supported(id: usize, v: u64) -> Result<bool, String> {
    // DXGKARG_ISSUPPORTEDVIDPN: hDesiredVidPn 0, IsVidPnSupported 8.
    let mut a = [0u8; 0x10];
    unsafe { wr::<u64>(a.as_mut_ptr(), 0, v) };
    let st = call2(id, super::DDI_IS_SUPPORTED_VIDPN, &mut a)?;
    if st < 0 {
        return Err(format!("DxgkDdiIsSupportedVidPn -> {:#010x}", st as u32));
    }
    Ok(a[8] != 0)
}

fn cofunc(id: usize, v: u64, pivot: u32, which: u32) -> Result<(), String> {
    // DXGKARG_ENUMVIDPNCOFUNCMODALITY: hConstrainingVidPn 0, EnumPivotType 8,
    // EnumPivot 0xC (VidPnSourceId / VidPnTargetId).
    let mut a = [0u8; 0x18];
    unsafe {
        wr::<u64>(a.as_mut_ptr(), 0, v);
        wr::<u32>(a.as_mut_ptr(), 8, pivot);
        wr::<u32>(a.as_mut_ptr(), 0xC, which);
    }
    let st = call2(id, super::DDI_ENUM_VIDPN_COFUNC_MODALITY, &mut a)?;
    if st < 0 {
        return Err(format!("DxgkDdiEnumVidPnCofuncModality -> {:#010x}", st as u32));
    }
    Ok(())
}

/// Pin the mode in set `s` that matches `w` x `h` (or the first), and
/// return it.
fn pin_matching(s: u64, w: u32, h: u32, target: bool) -> Option<usize> {
    let modes = locked(|o| o.sets.get(&s).map(|x| x.modes.clone())).unwrap_or_default();
    let size = |p: usize| unsafe {
        let p = p as *const u8;
        if target {
            (rd::<u32>(p, VSI + VSI_ACTIVE), rd::<u32>(p, VSI + VSI_ACTIVE + 4))
        } else {
            (rd::<u32>(p, SM_SIZE), rd::<u32>(p, SM_SIZE + 4))
        }
    };
    let pick = modes.iter().copied().find(|&p| size(p) == (w, h)).or(modes.first().copied())?;
    (set_pin_mode(s, mode_id(pick)) >= 0).then_some(pick)
}

fn commit_in(id: usize, v: u64, tgt: u32, w: u32, h: u32) -> Result<(), String> {
    if !is_supported(id, v)? {
        return Err(String::from("the driver doesn't support the topology"));
    }
    cofunc(id, v, 5 /* D3DKMDT_EPT_NOPIVOT */, 0)?;
    let (src_set, tgt_set) = locked(|o| {
        let x = o.vidpns.get(&v);
        (x.and_then(|x| x.sources.get(&0).copied()), x.and_then(|x| x.targets.get(&tgt).copied()))
    });
    let src_mode = src_set.and_then(|s| pin_matching(s, w, h, false)).ok_or("the driver offered no source mode")?;
    let (sw, sh, stride, format) = unsafe {
        let p = src_mode as *const u8;
        (rd::<u32>(p, SM_SIZE), rd::<u32>(p, SM_SIZE + 4), rd::<u32>(p, SM_STRIDE), rd::<u32>(p, SM_FORMAT))
    };
    let kind = unsafe { rd::<u32>(src_mode as *const u8, SM_TYPE) };
    super::note(id, format!("source mode pinned: {}x{} stride {} format {} (type {})", sw, sh, stride, format, kind));
    // Pinning the source narrows the targets: ask again with it as pivot.
    cofunc(id, v, 1 /* D3DKMDT_EPT_VIDPNSOURCE */, 0)?;
    let tgt_set = locked(|o| o.vidpns.get(&v).and_then(|x| x.targets.get(&tgt).copied())).or(tgt_set);
    let tgt_mode = tgt_set.and_then(|s| pin_matching(s, sw, sh, true)).ok_or("the driver offered no target mode")?;
    let (tw, th) = unsafe { (rd::<u32>(tgt_mode as *const u8, VSI + VSI_ACTIVE), rd::<u32>(tgt_mode as *const u8, VSI + VSI_ACTIVE + 4)) };
    super::note(id, format!("target mode pinned: {}x{}", tw, th));
    if !is_supported(id, v)? {
        return Err(String::from("the driver doesn't support the pinned modes"));
    }
    // DXGKARG_COMMITVIDPN: hFunctionalVidPn 0, AffectedVidPnSourceId 8,
    // MonitorConnectivityChecks 0xC, hPrimaryAllocation 0x10, Flags 0x18.
    let mut a = [0u8; 0x20];
    unsafe {
        wr::<u64>(a.as_mut_ptr(), 0, v);
        wr::<u32>(a.as_mut_ptr(), 8, 0);
        wr::<u32>(a.as_mut_ptr(), 0xC, 2); // D3DKMDT_MCC_ENFORCE
    }
    let st = call2(id, super::DDI_COMMIT_VIDPN, &mut a)?;
    super::note(id, format!("DxgkDdiCommitVidPn -> {:#010x}", st as u32));
    if st < 0 {
        return Err(format!("DxgkDdiCommitVidPn -> {:#010x}", st as u32));
    }
    let phys = with_adapter(id, |a| a.post.map_or(0, |p| p.phys)).unwrap_or(0);
    with_adapter(id, |a| a.mode = Some(Display { width: sw, height: sh, pitch: stride, format, phys, target: tgt }));
    Ok(())
}
