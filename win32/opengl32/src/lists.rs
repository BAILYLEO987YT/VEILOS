//! Display lists (WIN32_DESIGN.md G5b). A list is the calls made between
//! glNewList and glEndList, kept as closures that make the same call again
//! (with their arguments, and any data pointed at, copied when recorded).
//! Every call GL lets a list hold starts with `rec!`: while a list is being
//! compiled that records the call and returns (GL_COMPILE), or records it
//! and then runs it (GL_COMPILE_AND_EXECUTE). While a list plays back, or a
//! recorded call runs, `replaying` is above 0 and nothing is recorded, so a
//! call that is made of other calls (glTranslated is glTranslatef) is only
//! recorded once.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::{current, outside, with, GL_INVALID_ENUM, GL_INVALID_OPERATION, GL_INVALID_VALUE};

pub type Cmd = Box<dyn Fn()>;

const GL_COMPILE: u32 = 0x1300;
const GL_COMPILE_AND_EXECUTE: u32 = 0x1301;
/// GL's minimum for nested glCallList.
const MAX_NESTING: u32 = 64;

#[derive(Default)]
pub struct Lists {
    pub lists: BTreeMap<u32, Vec<Cmd>>,
    /// The list being compiled: its name, mode, and calls so far.
    pub compiling: Option<(u32, u32, Vec<Cmd>)>,
    pub replaying: u32,
    pub base: u32,
}

/// Record `cmd` if a list is being compiled. True if the caller is done
/// (the call was recorded, and run too if the list is also executed).
pub fn record(cmd: Cmd) -> bool {
    let Some(c) = current() else { return false };
    if c.lists.replaying > 0 {
        return false;
    }
    let Some((_, mode, cmds)) = c.lists.compiling.as_mut() else { return false };
    let mode = *mode;
    if mode == GL_COMPILE_AND_EXECUTE {
        c.lists.replaying += 1;
        cmd();
        // The call may have changed which context is current; ours stays.
        if let Some(c) = current() {
            c.lists.replaying -= 1;
            if let Some((_, _, cmds)) = c.lists.compiling.as_mut() {
                cmds.push(cmd);
            }
        }
    } else {
        cmds.push(cmd);
    }
    true
}

/// At the start of a call GL lets a display list hold.
macro_rules! rec {
    ($($call:tt)*) => {
        if $crate::lists::record(alloc::boxed::Box::new(move || {
            #[allow(unused_unsafe)]
            unsafe { $($call)*; }
        })) {
            return;
        }
    };
}
pub(crate) use rec;

#[no_mangle]
pub extern "system" fn glNewList(list: u32, mode: u32) {
    outside(|c| {
        if list == 0 {
            c.err(GL_INVALID_VALUE);
        } else if mode != GL_COMPILE && mode != GL_COMPILE_AND_EXECUTE {
            c.err(GL_INVALID_ENUM);
        } else if c.lists.compiling.is_some() {
            c.err(GL_INVALID_OPERATION);
        } else {
            c.lists.compiling = Some((list, mode, Vec::new()));
        }
    })
}

#[no_mangle]
pub extern "system" fn glEndList() {
    outside(|c| match c.lists.compiling.take() {
        Some((name, _, cmds)) => {
            c.lists.lists.insert(name, cmds);
        }
        None => c.err(GL_INVALID_OPERATION),
    })
}

#[no_mangle]
pub extern "system" fn glGenLists(range: i32) -> u32 {
    outside(|c| {
        if range < 0 {
            c.err(GL_INVALID_VALUE);
            return 0;
        }
        if range == 0 {
            return 0;
        }
        // The first run of `range` free names.
        let mut first = 1u32;
        loop {
            match (first..first + range as u32).find(|n| c.lists.lists.contains_key(n)) {
                Some(used) => first = used + 1,
                None => break,
            }
        }
        for n in first..first + range as u32 {
            c.lists.lists.insert(n, Vec::new());
        }
        first
    })
}

#[no_mangle]
pub extern "system" fn glDeleteLists(list: u32, range: i32) {
    outside(|c| {
        if range < 0 {
            c.err(GL_INVALID_VALUE);
            return;
        }
        for n in list..list.saturating_add(range as u32) {
            c.lists.lists.remove(&n);
        }
    })
}

#[no_mangle]
pub extern "system" fn glIsList(list: u32) -> u8 {
    outside(|c| c.lists.lists.contains_key(&list) as u8)
}

#[no_mangle]
pub extern "system" fn glListBase(base: u32) {
    rec!(glListBase(base));
    outside(|c| c.lists.base = base)
}

/// Play list `n` back.
fn call(n: u32) {
    // The list is taken out while it plays (so its calls can reach the
    // context) and put back after; a list calling itself finds it empty.
    let Some(cmds) = with(|c| {
        if c.lists.replaying >= MAX_NESTING {
            return None;
        }
        c.lists.replaying += 1;
        Some(c.lists.lists.remove(&n))
    }) else {
        return;
    };
    if let Some(cmds) = &cmds {
        for cmd in cmds {
            cmd();
        }
    }
    with(|c| {
        c.lists.replaying -= 1;
        if let Some(cmds) = cmds {
            c.lists.lists.entry(n).or_insert(cmds);
        }
    });
}

#[no_mangle]
pub extern "system" fn glCallList(list: u32) {
    rec!(glCallList(list));
    call(list);
}

/// glCallLists' names, as `kind` says they're stored.
unsafe fn names(n: i32, kind: u32, lists: *const u8) -> Option<Vec<u32>> {
    let n = n.max(0) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = unsafe {
            match kind {
                0x1400 => *(lists as *const i8).add(i) as i32 as u32,
                0x1401 => *lists.add(i) as u32,
                0x1402 => core::ptr::read_unaligned((lists as *const i16).add(i)) as i32 as u32,
                0x1403 => core::ptr::read_unaligned((lists as *const u16).add(i)) as u32,
                0x1404 | 0x1405 => core::ptr::read_unaligned((lists as *const u32).add(i)),
                0x1406 => core::ptr::read_unaligned((lists as *const f32).add(i)) as u32,
                // GL_2_BYTES, GL_3_BYTES, GL_4_BYTES: big-endian runs.
                0x1407 => (*lists.add(2 * i) as u32) << 8 | *lists.add(2 * i + 1) as u32,
                0x1408 => (*lists.add(3 * i) as u32) << 16 | (*lists.add(3 * i + 1) as u32) << 8 | *lists.add(3 * i + 2) as u32,
                0x1409 => u32::from_be_bytes([*lists.add(4 * i), *lists.add(4 * i + 1), *lists.add(4 * i + 2), *lists.add(4 * i + 3)]),
                _ => return None,
            }
        };
        out.push(v);
    }
    Some(out)
}

#[no_mangle]
pub unsafe extern "system" fn glCallLists(n: i32, kind: u32, lists: *const u8) {
    let Some(names) = (unsafe { names(n, kind, lists) }) else {
        with(|c| c.err(GL_INVALID_ENUM));
        return;
    };
    let rec_names = names.clone();
    rec!(call_lists(&rec_names));
    call_lists(&names);
}

fn call_lists(names: &[u32]) {
    let base = with(|c| c.lists.base);
    for &n in names {
        call(base.wrapping_add(n));
    }
}
