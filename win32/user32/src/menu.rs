//! Menus and accelerators: menu objects (built by hand or from MENU and
//! MENUEX resources), a top-level window's menu bar (drawn in its pixels
//! below the desktop's caption, like the scroll bars), popup menus in
//! "#32768" popup windows, the menu loop (mouse and keyboard, the messages
//! Windows sends in its order), the system menu, keyboard menus (Alt+key,
//! Alt, F10, Alt+Space) and accelerator tables.

use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::message::send;
use crate::{state, Bool, Hwnd};

pub const MF_GRAYED: u32 = 0x0001;
pub const MF_DISABLED: u32 = 0x0002;
const MF_BITMAP: u32 = 0x0004;
pub const MF_CHECKED: u32 = 0x0008;
pub const MF_POPUP: u32 = 0x0010;
const MF_MENUBARBREAK: u32 = 0x0020;
const MF_MENUBREAK: u32 = 0x0040;
pub const MF_HILITE: u32 = 0x0080;
const MF_OWNERDRAW: u32 = 0x0100;
const MF_RADIOCHECK: u32 = 0x0200;
pub const MF_BYPOSITION: u32 = 0x0400;
pub const MF_SEPARATOR: u32 = 0x0800;
pub const MF_DEFAULT: u32 = 0x1000;
const MF_SYSMENU: u32 = 0x2000;
const MF_RIGHTJUSTIFY: u32 = 0x4000;

/// Type bits (MFT_*) and state bits (MFS_*) as AppendMenu's MF_* flags
/// carry them.
const TYPE_BITS: u32 = MF_BITMAP | MF_MENUBARBREAK | MF_MENUBREAK | MF_OWNERDRAW | MF_RADIOCHECK | MF_SEPARATOR | MF_RIGHTJUSTIFY;
const STATE_BITS: u32 = MF_GRAYED | MF_DISABLED | MF_CHECKED | MF_HILITE | MF_DEFAULT;

const WM_INITMENU: u32 = 0x0116;
const WM_INITMENUPOPUP: u32 = 0x0117;
const WM_MENUSELECT: u32 = 0x011F;
const WM_ENTERMENULOOP: u32 = 0x0211;
const WM_EXITMENULOOP: u32 = 0x0212;
const WM_UNINITMENUPOPUP: u32 = 0x0125;
const WM_COMMAND: u32 = 0x0111;
const WM_SYSCOMMAND: u32 = 0x0112;

/// SM_CYMENU: the menu bar's height (one row).
pub const BAR_H: i32 = 20;

#[derive(Clone)]
pub struct Item {
    pub ty: u32,
    pub state: u32,
    pub id: u32,
    pub sub: u64,
    pub text: Vec<u16>,
    pub data: u64,
}

pub struct Menu {
    pub items: Vec<Item>,
}

static MENUS: Lock<Vec<Option<Menu>>> = Lock::new(Vec::new());
const MENU_BASE: u64 = 0x0005_0000;

fn slot(h: u64) -> Option<usize> {
    let d = h.checked_sub(MENU_BASE)?;
    (d % 4 == 0).then_some((d / 4) as usize)
}

fn new_menu() -> u64 {
    let mut v = MENUS.lock();
    let i = match v.iter().position(|m| m.is_none()) {
        Some(i) => i,
        None => {
            v.push(None);
            v.len() - 1
        }
    };
    v[i] = Some(Menu { items: Vec::new() });
    MENU_BASE + 4 * i as u64
}

fn with_menu<R>(h: u64, f: impl FnOnce(&mut Menu) -> R) -> Option<R> {
    let mut v = MENUS.lock();
    v.get_mut(slot(h)?)?.as_mut().map(f)
}

fn items_of(h: u64) -> Vec<Item> {
    with_menu(h, |m| m.items.clone()).unwrap_or_default()
}

/// Find `item` (a command, searched in submenus too; or a position):
/// the menu holding it and its index.
fn find(h: u64, item: u32, flags: u32) -> Option<(u64, usize)> {
    let items = items_of(h);
    if flags & MF_BYPOSITION != 0 {
        return ((item as usize) < items.len()).then_some((h, item as usize));
    }
    if let Some(i) = items.iter().position(|it| it.sub == 0 && it.id == item) {
        return Some((h, i));
    }
    for it in &items {
        if it.sub != 0 {
            if let Some(r) = find(it.sub, item, flags) {
                return Some(r);
            }
        }
    }
    // A popup item can be named by its id too.
    items.iter().position(|it| it.id == item && it.sub != 0).map(|i| (h, i))
}

fn get_item(h: u64, i: usize) -> Option<Item> {
    with_menu(h, |m| m.items.get(i).cloned()).flatten()
}

fn set_item(h: u64, i: usize, f: impl FnOnce(&mut Item)) -> bool {
    with_menu(h, |m| m.items.get_mut(i).map(f).is_some()).unwrap_or(false)
}

/// MF_* flags and a string or submenu argument as an item.
fn make_item(flags: u32, id: u64, text: Vec<u16>) -> Item {
    let popup = flags & MF_POPUP != 0;
    let mut ty = flags & TYPE_BITS;
    let mut state = flags & STATE_BITS;
    if ty & MF_SEPARATOR != 0 {
        state |= MF_GRAYED | MF_DISABLED;
    }
    if text.is_empty() && !popup && id == 0 && flags & (MF_BITMAP | MF_OWNERDRAW) == 0 {
        ty |= MF_SEPARATOR;
        state |= MF_GRAYED | MF_DISABLED;
    }
    Item { ty, state, id: if popup { u32::MAX } else { id as u32 }, sub: if popup { id } else { 0 }, text, data: 0 }
}

fn text_arg(flags: u32, p: u64, unicode: bool) -> Vec<u16> {
    if flags & (MF_BITMAP | MF_OWNERDRAW | MF_SEPARATOR) != 0 || p >> 16 == 0 {
        return Vec::new();
    }
    if unicode { unsafe { crate::wstr(p as *const u16) } } else { unsafe { crate::from_ansi(p as *const u8, -1) } }
}

// -----| building menus |-----

#[no_mangle]
pub extern "system" fn CreateMenu() -> u64 {
    new_menu()
}

#[no_mangle]
pub extern "system" fn CreatePopupMenu() -> u64 {
    new_menu()
}

#[no_mangle]
pub extern "system" fn DestroyMenu(h: u64) -> Bool {
    let Some(i) = slot(h) else { return 0 };
    let subs: Vec<u64> = match MENUS.lock().get_mut(i).and_then(|m| m.take()) {
        Some(m) => m.items.iter().filter(|it| it.sub != 0).map(|it| it.sub).collect(),
        None => return 0,
    };
    for s in subs {
        DestroyMenu(s);
    }
    1
}

#[no_mangle]
pub extern "system" fn IsMenu(h: u64) -> Bool {
    with_menu(h, |_| ()).is_some() as Bool
}

fn insert(h: u64, at: Option<(u32, u32)>, item: Item) -> Bool {
    let place = match at {
        None => None,
        Some((pos, flags)) => match find(h, pos, flags) {
            Some(p) => Some(p),
            // Past the end by position: append.
            None if flags & MF_BYPOSITION != 0 => None,
            None => return 0,
        },
    };
    let (menu, i) = place.unwrap_or((h, usize::MAX));
    with_menu(menu, |m| {
        let i = i.min(m.items.len());
        m.items.insert(i, item);
    })
    .is_some() as Bool
}

#[no_mangle]
pub extern "system" fn AppendMenuW(h: u64, flags: u32, id: u64, text: u64) -> Bool {
    insert(h, None, make_item(flags, id, text_arg(flags, text, true)))
}

#[no_mangle]
pub extern "system" fn AppendMenuA(h: u64, flags: u32, id: u64, text: u64) -> Bool {
    insert(h, None, make_item(flags, id, text_arg(flags, text, false)))
}

#[no_mangle]
pub extern "system" fn InsertMenuW(h: u64, pos: u32, flags: u32, id: u64, text: u64) -> Bool {
    insert(h, Some((pos, flags)), make_item(flags, id, text_arg(flags, text, true)))
}

#[no_mangle]
pub extern "system" fn InsertMenuA(h: u64, pos: u32, flags: u32, id: u64, text: u64) -> Bool {
    insert(h, Some((pos, flags)), make_item(flags, id, text_arg(flags, text, false)))
}

fn modify(h: u64, pos: u32, flags: u32, id: u64, text: Vec<u16>) -> Bool {
    let Some((m, i)) = find(h, pos, flags) else { return 0 };
    let new = make_item(flags & !MF_BYPOSITION, id, text);
    set_item(m, i, |it| {
        let data = it.data;
        *it = new;
        it.data = data;
    }) as Bool
}

#[no_mangle]
pub extern "system" fn ModifyMenuW(h: u64, pos: u32, flags: u32, id: u64, text: u64) -> Bool {
    modify(h, pos, flags, id, text_arg(flags, text, true))
}

#[no_mangle]
pub extern "system" fn ModifyMenuA(h: u64, pos: u32, flags: u32, id: u64, text: u64) -> Bool {
    modify(h, pos, flags, id, text_arg(flags, text, false))
}

fn remove(h: u64, pos: u32, flags: u32, destroy: bool) -> Bool {
    let Some((m, i)) = find(h, pos, flags) else { return 0 };
    let item = with_menu(m, |mm| mm.items.remove(i));
    if let (true, Some(it)) = (destroy, item) {
        if it.sub != 0 {
            DestroyMenu(it.sub);
        }
    }
    1
}

#[no_mangle]
pub extern "system" fn DeleteMenu(h: u64, pos: u32, flags: u32) -> Bool {
    remove(h, pos, flags, true)
}

#[no_mangle]
pub extern "system" fn RemoveMenu(h: u64, pos: u32, flags: u32) -> Bool {
    remove(h, pos, flags, false)
}

// -----| asking about menus |-----

#[no_mangle]
pub extern "system" fn GetMenuItemCount(h: u64) -> i32 {
    with_menu(h, |m| m.items.len() as i32).unwrap_or(-1)
}

#[no_mangle]
pub extern "system" fn GetMenuItemID(h: u64, pos: i32) -> u32 {
    match get_item(h, pos as usize) {
        Some(it) if it.sub != 0 => u32::MAX,
        Some(it) => it.id,
        None => u32::MAX,
    }
}

#[no_mangle]
pub extern "system" fn GetSubMenu(h: u64, pos: i32) -> u64 {
    get_item(h, pos as usize).map_or(0, |it| it.sub)
}

/// An item's type and state as MF_* flags; a popup's also give its item
/// count in the high byte.
fn state_of(it: &Item) -> u32 {
    let mut s = it.ty | it.state;
    if it.sub != 0 {
        s |= MF_POPUP | ((GetMenuItemCount(it.sub).max(0) as u32) << 8);
    }
    s
}

#[no_mangle]
pub extern "system" fn GetMenuState(h: u64, item: u32, flags: u32) -> u32 {
    match find(h, item, flags).and_then(|(m, i)| get_item(m, i)) {
        Some(it) => state_of(&it),
        None => u32::MAX,
    }
}

fn menu_string(h: u64, item: u32, flags: u32) -> Option<Vec<u16>> {
    find(h, item, flags).and_then(|(m, i)| get_item(m, i)).map(|it| it.text)
}

#[no_mangle]
pub unsafe extern "system" fn GetMenuStringW(h: u64, item: u32, out: *mut u16, cap: i32, flags: u32) -> i32 {
    let Some(t) = menu_string(h, item, flags) else { return 0 };
    if out.is_null() || cap <= 0 {
        return t.len() as i32;
    }
    unsafe { crate::copy_out(&t, out, cap as usize) as i32 }
}

#[no_mangle]
pub unsafe extern "system" fn GetMenuStringA(h: u64, item: u32, out: *mut u8, cap: i32, flags: u32) -> i32 {
    let Some(t) = menu_string(h, item, flags) else { return 0 };
    let a = crate::to_ansi(&t);
    if out.is_null() || cap <= 0 {
        return a.len() as i32;
    }
    unsafe { crate::copy_out(&a, out, cap as usize) as i32 }
}

#[no_mangle]
pub extern "system" fn CheckMenuItem(h: u64, item: u32, flags: u32) -> u32 {
    let Some((m, i)) = find(h, item, flags) else { return u32::MAX };
    let mut old = 0;
    set_item(m, i, |it| {
        old = it.state & MF_CHECKED;
        it.state = (it.state & !MF_CHECKED) | (flags & MF_CHECKED);
    });
    old
}

#[no_mangle]
pub extern "system" fn EnableMenuItem(h: u64, item: u32, flags: u32) -> u32 {
    let Some((m, i)) = find(h, item, flags) else { return u32::MAX };
    let mut old = 0;
    set_item(m, i, |it| {
        old = it.state & (MF_GRAYED | MF_DISABLED);
        it.state = (it.state & !(MF_GRAYED | MF_DISABLED)) | (flags & (MF_GRAYED | MF_DISABLED));
    });
    redraw_bars(h);
    old
}

/// Check one item of a range as a radio item and uncheck the others.
#[no_mangle]
pub extern "system" fn CheckMenuRadioItem(h: u64, first: u32, last: u32, check: u32, flags: u32) -> Bool {
    let mut done = false;
    for id in first..=last {
        if let Some((m, i)) = find(h, id, flags) {
            set_item(m, i, |it| {
                if id == check {
                    it.state |= MF_CHECKED;
                    it.ty |= MF_RADIOCHECK;
                    done = true;
                } else {
                    it.state &= !MF_CHECKED;
                }
            });
        }
    }
    done as Bool
}

#[no_mangle]
pub extern "system" fn HiliteMenuItem(_hwnd: Hwnd, h: u64, item: u32, flags: u32) -> Bool {
    let Some((m, i)) = find(h, item, flags) else { return 0 };
    set_item(m, i, |it| it.state = (it.state & !MF_HILITE) | (flags & MF_HILITE)) as Bool
}

#[no_mangle]
pub extern "system" fn SetMenuDefaultItem(h: u64, item: u32, by_pos: u32) -> Bool {
    let items = items_of(h);
    let i = if item == u32::MAX { None } else if by_pos != 0 { Some(item as usize) } else { items.iter().position(|it| it.id == item) };
    with_menu(h, |m| {
        for (k, it) in m.items.iter_mut().enumerate() {
            if Some(k) == i { it.state |= MF_DEFAULT } else { it.state &= !MF_DEFAULT }
        }
    })
    .is_some() as Bool
}

#[no_mangle]
pub extern "system" fn GetMenuDefaultItem(h: u64, by_pos: u32, _flags: u32) -> u32 {
    let items = items_of(h);
    match items.iter().position(|it| it.state & MF_DEFAULT != 0) {
        Some(i) if by_pos != 0 => i as u32,
        Some(i) => items[i].id,
        None => u32::MAX,
    }
}

#[no_mangle]
pub extern "system" fn GetMenuCheckMarkDimensions() -> i32 {
    13 << 16 | 13
}

#[no_mangle]
pub extern "system" fn SetMenuItemBitmaps(_h: u64, _item: u32, _flags: u32, _unchecked: u64, _checked: u64) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn GetMenuInfo(_h: u64, _mi: *mut u8) -> Bool {
    1
}

#[no_mangle]
pub extern "system" fn SetMenuInfo(_h: u64, _mi: *const u8) -> Bool {
    1
}

/// MENUITEMINFO: cbSize, fMask, fType, fState, wID, hSubMenu,
/// hbmpChecked, hbmpUnchecked, dwItemData, dwTypeData, cch, hbmpItem.
const MIIM_STATE: u32 = 0x01;
const MIIM_ID: u32 = 0x02;
const MIIM_SUBMENU: u32 = 0x04;
const MIIM_TYPE: u32 = 0x10;
const MIIM_DATA: u32 = 0x20;
const MIIM_STRING: u32 = 0x40;
const MIIM_FTYPE: u32 = 0x100;

unsafe fn get_info(h: u64, item: u32, by_pos: Bool, mi: *mut u8, unicode: bool) -> Bool {
    let flags = if by_pos != 0 { MF_BYPOSITION } else { 0 };
    let Some(it) = find(h, item, flags).and_then(|(m, i)| get_item(m, i)) else { return 0 };
    let rd32 = |o: usize| unsafe { core::ptr::read_unaligned(mi.add(o) as *const u32) };
    let wr32 = |o: usize, v: u32| unsafe { core::ptr::write_unaligned(mi.add(o) as *mut u32, v) };
    let wr64 = |o: usize, v: u64| unsafe { core::ptr::write_unaligned(mi.add(o) as *mut u64, v) };
    let mask = rd32(4);
    if mask & (MIIM_FTYPE | MIIM_TYPE) != 0 {
        wr32(8, it.ty);
    }
    if mask & MIIM_STATE != 0 {
        wr32(12, it.state);
    }
    if mask & MIIM_ID != 0 {
        wr32(16, if it.sub != 0 && it.id == u32::MAX { 0 } else { it.id });
    }
    if mask & MIIM_SUBMENU != 0 {
        wr64(24, it.sub);
    }
    if mask & MIIM_DATA != 0 {
        wr64(48, it.data);
    }
    if mask & (MIIM_STRING | MIIM_TYPE) != 0 && it.ty & (MF_SEPARATOR | MF_BITMAP | MF_OWNERDRAW) == 0 {
        let out = unsafe { core::ptr::read_unaligned(mi.add(56) as *const u64) };
        let cap = rd32(64) as usize;
        let n = if unicode {
            if out != 0 && cap > 0 { unsafe { crate::copy_out(&it.text, out as *mut u16, cap) } } else { it.text.len() }
        } else {
            let a = crate::to_ansi(&it.text);
            if out != 0 && cap > 0 { unsafe { crate::copy_out(&a, out as *mut u8, cap) } } else { a.len() }
        };
        wr32(64, n as u32);
    }
    1
}

unsafe fn set_info(h: u64, item: u32, by_pos: Bool, mi: *const u8, unicode: bool, insert_new: bool) -> Bool {
    let rd32 = |o: usize| unsafe { core::ptr::read_unaligned(mi.add(o) as *const u32) };
    let rd64 = |o: usize| unsafe { core::ptr::read_unaligned(mi.add(o) as *const u64) };
    let mask = rd32(4);
    let mut it = Item { ty: 0, state: 0, id: 0, sub: 0, text: Vec::new(), data: 0 };
    let flags = if by_pos != 0 { MF_BYPOSITION } else { 0 };
    if !insert_new {
        match find(h, item, flags).and_then(|(m, i)| get_item(m, i)) {
            Some(old) => it = old,
            None => return 0,
        }
    }
    if mask & (MIIM_FTYPE | MIIM_TYPE) != 0 {
        it.ty = rd32(8);
    }
    if mask & MIIM_STATE != 0 {
        it.state = rd32(12);
    }
    if mask & MIIM_ID != 0 {
        it.id = rd32(16);
    }
    if mask & MIIM_SUBMENU != 0 {
        it.sub = rd64(24);
    }
    if mask & MIIM_DATA != 0 {
        it.data = rd64(48);
    }
    if mask & (MIIM_STRING | MIIM_TYPE) != 0 && it.ty & (MF_SEPARATOR | MF_BITMAP | MF_OWNERDRAW) == 0 {
        let p = rd64(56);
        it.text = if p >> 16 == 0 { Vec::new() } else if unicode { unsafe { crate::wstr(p as *const u16) } } else { unsafe { crate::from_ansi(p as *const u8, -1) } };
    }
    if it.ty & MF_SEPARATOR != 0 {
        it.state |= MF_GRAYED | MF_DISABLED;
    }
    if it.state & MF_DEFAULT != 0 {
        // Only one default item.
        with_menu(h, |m| m.items.iter_mut().for_each(|x| x.state &= !MF_DEFAULT));
    }
    if insert_new {
        return insert(h, Some((item, flags)), it);
    }
    let Some((m, i)) = find(h, item, flags) else { return 0 };
    set_item(m, i, |x| *x = it) as Bool
}

#[no_mangle]
pub unsafe extern "system" fn GetMenuItemInfoW(h: u64, item: u32, by_pos: Bool, mi: *mut u8) -> Bool {
    unsafe { get_info(h, item, by_pos, mi, true) }
}

#[no_mangle]
pub unsafe extern "system" fn GetMenuItemInfoA(h: u64, item: u32, by_pos: Bool, mi: *mut u8) -> Bool {
    unsafe { get_info(h, item, by_pos, mi, false) }
}

#[no_mangle]
pub unsafe extern "system" fn SetMenuItemInfoW(h: u64, item: u32, by_pos: Bool, mi: *const u8) -> Bool {
    unsafe { set_info(h, item, by_pos, mi, true, false) }
}

#[no_mangle]
pub unsafe extern "system" fn SetMenuItemInfoA(h: u64, item: u32, by_pos: Bool, mi: *const u8) -> Bool {
    unsafe { set_info(h, item, by_pos, mi, false, false) }
}

#[no_mangle]
pub unsafe extern "system" fn InsertMenuItemW(h: u64, item: u32, by_pos: Bool, mi: *const u8) -> Bool {
    unsafe { set_info(h, item, by_pos, mi, true, true) }
}

#[no_mangle]
pub unsafe extern "system" fn InsertMenuItemA(h: u64, item: u32, by_pos: Bool, mi: *const u8) -> Bool {
    unsafe { set_info(h, item, by_pos, mi, false, true) }
}

// -----| menu resources |-----

struct Rd<'a> {
    d: &'a [u8],
    at: usize,
}

impl Rd<'_> {
    fn u16(&mut self) -> u16 {
        let v = self.d.get(self.at..self.at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        self.at += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = self.d.get(self.at..self.at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        self.at += 4;
        v
    }
    fn string(&mut self) -> Vec<u16> {
        let mut s = Vec::new();
        while self.at < self.d.len() {
            let c = self.u16();
            if c == 0 {
                break;
            }
            s.push(c);
        }
        s
    }
}

/// A MENU template's items (to the one marked MF_END) into `h`.
fn parse_v0(r: &mut Rd, h: u64) {
    while r.at < r.d.len() {
        let flags = r.u16() as u32;
        let popup = flags & MF_POPUP != 0;
        let id = if popup { 0 } else { r.u16() as u64 };
        let text = r.string();
        let item_flags = flags & !(0x80 | MF_POPUP);
        if popup {
            let sub = new_menu();
            parse_v0(r, sub);
            insert(h, None, make_item(item_flags | MF_POPUP, sub, text));
        } else {
            insert(h, None, make_item(item_flags, id, text));
        }
        if flags & 0x80 != 0 {
            return;
        }
    }
}

/// A MENUEX template's items into `h`.
fn parse_v1(r: &mut Rd, h: u64) {
    while r.at < r.d.len() {
        let ty = r.u32();
        let st = r.u32();
        let id = r.u32();
        let res = r.u16();
        let text = r.string();
        r.at = (r.at + 3) & !3;
        let mut it = Item { ty, state: st, id, sub: 0, text, data: 0 };
        if ty & MF_SEPARATOR != 0 {
            it.state |= MF_GRAYED | MF_DISABLED;
        }
        if res & 0x01 != 0 {
            let _help = r.u32();
            let sub = new_menu();
            parse_v1(r, sub);
            it.sub = sub;
            it.id = id;
        }
        insert(h, None, it);
        if res & 0x80 != 0 {
            return;
        }
    }
}

fn load_template(d: &[u8]) -> u64 {
    let mut r = Rd { d, at: 0 };
    let version = r.u16();
    let offset = r.u16() as usize;
    let h = new_menu();
    if version == 1 {
        r.at = 4 + offset;
        parse_v1(&mut r, h);
    } else {
        r.at = 4 + offset;
        parse_v0(&mut r, h);
    }
    h
}

fn resource(inst: u64, name: *const u16, kind: u16) -> Option<&'static [u8]> {
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn FindResourceW(module: u64, name: *const u16, kind: *const u16) -> u64;
        fn LoadResource(module: u64, res: u64) -> u64;
        fn LockResource(data: u64) -> *const u8;
        fn SizeofResource(module: u64, res: u64) -> u32;
        fn GetModuleHandleW(name: *const u16) -> u64;
    }
    let inst = if inst == 0 { unsafe { GetModuleHandleW(core::ptr::null()) } } else { inst };
    let res = unsafe { FindResourceW(inst, name, kind as usize as *const u16) };
    if res == 0 {
        return None;
    }
    let size = unsafe { SizeofResource(inst, res) } as usize;
    let p = unsafe { LockResource(LoadResource(inst, res)) };
    (!p.is_null()).then(|| unsafe { core::slice::from_raw_parts(p, size) })
}

unsafe fn name_a(p: *const u8) -> (Vec<u16>, *const u16) {
    if (p as usize) >> 16 == 0 {
        return (Vec::new(), p as *const u16);
    }
    let mut w = unsafe { crate::from_ansi(p, -1) };
    w.push(0);
    let ptr = w.as_ptr();
    (w, ptr)
}

#[no_mangle]
pub extern "system" fn LoadMenuW(inst: u64, name: *const u16) -> u64 {
    resource(inst, name, 4).map_or(0, load_template) // RT_MENU
}

#[no_mangle]
pub unsafe extern "system" fn LoadMenuA(inst: u64, name: *const u8) -> u64 {
    let (_k, w) = unsafe { name_a(name) };
    LoadMenuW(inst, w)
}

#[no_mangle]
pub unsafe extern "system" fn LoadMenuIndirectW(t: *const u8) -> u64 {
    if t.is_null() {
        return 0;
    }
    load_template(unsafe { core::slice::from_raw_parts(t, 0x10000) })
}

#[no_mangle]
pub unsafe extern "system" fn LoadMenuIndirectA(t: *const u8) -> u64 {
    unsafe { LoadMenuIndirectW(t) }
}

// -----| a window's menu bar |-----

#[no_mangle]
pub extern "system" fn GetMenu(hwnd: Hwnd) -> u64 {
    state().get(hwnd).filter(|w| w.parent == 0).map_or(0, |w| w.menu)
}

#[no_mangle]
pub extern "system" fn SetMenu(hwnd: Hwnd, menu: u64) -> Bool {
    match state().get_mut(hwnd) {
        Some(w) if w.parent == 0 => w.menu = menu,
        _ => return 0,
    }
    // The non-client area changes: SWP_FRAMECHANGED.
    crate::window::set_window_pos(hwnd, 0, 0, 0, 0, 0, 0x0020 | 0x1 | 0x2 | 0x4 | 0x10);
    1
}

#[no_mangle]
pub extern "system" fn DrawMenuBar(hwnd: Hwnd) -> Bool {
    crate::paint::paint_frame(hwnd);
    1
}

fn redraw_bars(menu: u64) {
    let wins: Vec<Hwnd> = state().windows.iter().filter(|w| w.menu == menu && w.parent == 0).map(|w| w.hwnd).collect();
    for w in wins {
        crate::paint::paint_frame(w);
    }
}

static MENU_FONT: Lock<(u64, u64)> = Lock::new((0, 0));

/// The menu font (Windows' is Segoe UI 9 point), and its bold form for
/// default items.
fn fonts() -> (u64, u64) {
    let mut f = MENU_FONT.lock();
    if f.0 == 0 {
        let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
        f.0 = unsafe { crate::CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 0, 0, face.as_ptr()) };
        f.1 = unsafe { crate::CreateFontW(-12, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 0, 0, face.as_ptr()) };
    }
    *f
}

fn text_w(hdc: u64, s: &[u16]) -> i32 {
    // Without the '&' prefixes.
    let clean: Vec<u16> = s.iter().copied().filter(|&c| c != b'&' as u16).collect();
    let mut sz = [0i32; 2];
    unsafe { crate::GetTextExtentPoint32W(hdc, clean.as_ptr(), clean.len() as i32, &mut sz) };
    sz[0]
}

/// The menu bar's rectangle in window coordinates.
fn bar_rect(hwnd: Hwnd) -> Option<[i32; 4]> {
    let s = state();
    let w = s.get(hwnd).filter(|w| w.parent == 0 && w.menu != 0)?;
    let f = crate::window::frame_margins(w.style, w.ex_style, true);
    Some([f[0], f[1], w.w - f[2], f[1] + BAR_H])
}

/// Each bar item's rectangle in window coordinates.
fn bar_items(hwnd: Hwnd) -> Vec<[i32; 4]> {
    let Some(r) = bar_rect(hwnd) else { return Vec::new() };
    let menu = GetMenu(hwnd);
    let hdc = unsafe { crate::CreateCompatibleDC(0) };
    let old = unsafe { crate::SelectObject(hdc, fonts().0) };
    let mut x = r[0];
    let out = items_of(menu)
        .iter()
        .map(|it| {
            let w = text_w(hdc, &it.text) + 14;
            let rr = [x, r[1], x + w, r[3]];
            x += w;
            rr
        })
        .collect();
    unsafe {
        crate::SelectObject(hdc, old);
        crate::DeleteDC(hdc);
    }
    out
}

/// The bar item (window, index) highlighted by the menu loop.
static HOT_BAR: Lock<(Hwnd, Option<usize>)> = Lock::new((0, None));

/// Draw `hwnd`'s menu bar on its window DC.
pub fn paint_bar(hwnd: Hwnd, hdc: u64) {
    let Some(r) = bar_rect(hwnd) else { return };
    let menu = GetMenu(hwnd);
    crate::paint::FillRect(hdc, &r, crate::paint::GetSysColorBrush(30));
    let hot = HOT_BAR.lock();
    let hot = if hot.0 == hwnd { hot.1 } else { None };
    let old = unsafe { crate::SelectObject(hdc, fonts().0) };
    unsafe { crate::SetBkMode(hdc, 1) };
    for (i, (it, ir)) in items_of(menu).iter().zip(bar_items(hwnd)).enumerate() {
        if hot == Some(i) {
            crate::paint::FillRect(hdc, &ir, crate::paint::GetSysColorBrush(13));
        }
        let color = if it.state & MF_GRAYED != 0 { 17 } else if hot == Some(i) { 14 } else { 7 };
        let oc = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(color)) };
        let mut t = it.text.clone();
        let mut rr = ir;
        unsafe { crate::paint::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, 0x25) };
        unsafe { crate::SetTextColor(hdc, oc) };
    }
    unsafe { crate::SelectObject(hdc, old) };
}

/// Hit test: is window point (x, y) on the menu bar?
pub fn on_bar(hwnd: Hwnd, x: i32, y: i32) -> bool {
    bar_rect(hwnd).is_some_and(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
}

#[no_mangle]
pub extern "system" fn GetMenuItemRect(hwnd: Hwnd, menu: u64, item: u32, out: *mut [i32; 4]) -> Bool {
    if out.is_null() {
        return 0;
    }
    let mut wr = [0i32; 4];
    crate::window::GetWindowRect(hwnd, &mut wr);
    if GetMenu(hwnd) == menu {
        if let Some(r) = bar_items(hwnd).get(item as usize) {
            unsafe { *out = [wr[0] + r[0], wr[1] + r[1], wr[0] + r[2], wr[1] + r[3]] };
            return 1;
        }
    }
    0
}

// -----| popup menu windows |-----

const SEP_H: i32 = 9;
const CHECK_W: i32 = 22;
const ARROW_W: i32 = 18;

/// (font height) the height of a text item.
fn item_h() -> i32 {
    let hdc = unsafe { crate::CreateCompatibleDC(0) };
    let old = unsafe { crate::SelectObject(hdc, fonts().0) };
    let mut tm = [0u8; 60];
    unsafe { crate::GetTextMetricsW(hdc, tm.as_mut_ptr()) };
    unsafe {
        crate::SelectObject(hdc, old);
        crate::DeleteDC(hdc);
    }
    i32::from_le_bytes(tm[0..4].try_into().unwrap()) + 6
}

/// The text before and after the tab (label, shortcut).
fn split_tab(t: &[u16]) -> (&[u16], &[u16]) {
    match t.iter().position(|&c| c == 9) {
        Some(i) => (&t[..i], &t[i + 1..]),
        None => (t, &[]),
    }
}

/// A popup menu's size, and its items' rectangles (client coordinates).
fn popup_layout(menu: u64) -> (i32, i32, Vec<[i32; 4]>) {
    let items = items_of(menu);
    let h = item_h();
    let hdc = unsafe { crate::CreateCompatibleDC(0) };
    let old = unsafe { crate::SelectObject(hdc, fonts().1) };
    let (mut label_w, mut key_w) = (0, 0);
    for it in &items {
        let (l, k) = split_tab(&it.text);
        label_w = label_w.max(text_w(hdc, l));
        key_w = key_w.max(text_w(hdc, k));
    }
    unsafe {
        crate::SelectObject(hdc, old);
        crate::DeleteDC(hdc);
    }
    let width = CHECK_W + label_w + if key_w > 0 { 24 + key_w } else { 0 } + ARROW_W + 4;
    let mut y = 2;
    let rects = items
        .iter()
        .map(|it| {
            let ih = if it.ty & MF_SEPARATOR != 0 { SEP_H } else { h };
            let r = [2, y, width - 2, y + ih];
            y += ih;
            r
        })
        .collect();
    (width, y + 2, rects)
}

/// Each open popup window's menu and highlighted item.
static POPUPS: Lock<Vec<(Hwnd, u64, Option<usize>)>> = Lock::new(Vec::new());

fn popup_of(win: Hwnd) -> Option<(u64, Option<usize>)> {
    POPUPS.lock().iter().find(|p| p.0 == win).map(|p| (p.1, p.2))
}

fn paint_popup(win: Hwnd, hdc: u64) {
    let Some((menu, sel)) = popup_of(win) else { return };
    let r = crate::controls::client(win);
    crate::controls::fill(hdc, r, 4);
    crate::controls::edge(hdc, r, false);
    let (_, _, rects) = popup_layout(menu);
    let (normal, bold) = fonts();
    unsafe { crate::SetBkMode(hdc, 1) };
    for (i, (it, ir)) in items_of(menu).iter().zip(rects).enumerate() {
        if it.ty & MF_SEPARATOR != 0 {
            let y = (ir[1] + ir[3]) / 2;
            crate::controls::fill(hdc, [ir[0] + 1, y - 1, ir[2] - 1, y], 16);
            crate::controls::fill(hdc, [ir[0] + 1, y, ir[2] - 1, y + 1], 20);
            continue;
        }
        let hot = sel == Some(i);
        if hot {
            crate::paint::FillRect(hdc, &ir, crate::paint::GetSysColorBrush(13));
        }
        let grayed = it.state & MF_GRAYED != 0;
        let color = if grayed { 17 } else if hot { 14 } else { 7 };
        let oc = unsafe { crate::SetTextColor(hdc, crate::paint::GetSysColor(color)) };
        let old = unsafe { crate::SelectObject(hdc, if it.state & MF_DEFAULT != 0 { bold } else { normal }) };
        let (label, key) = split_tab(&it.text);
        let mut l = label.to_vec();
        let mut lr = [ir[0] + CHECK_W, ir[1], ir[2] - ARROW_W, ir[3]];
        unsafe { crate::paint::DrawTextW(hdc, l.as_mut_ptr(), l.len() as i32, &mut lr, 0x24) };
        if !key.is_empty() {
            let mut k = key.to_vec();
            let mut kr = [ir[0] + CHECK_W, ir[1], ir[2] - ARROW_W, ir[3]];
            unsafe { crate::paint::DrawTextW(hdc, k.as_mut_ptr(), k.len() as i32, &mut kr, 0x26 | 0x800) };
        }
        unsafe { crate::SelectObject(hdc, old) };
        let mark = if grayed { 17 } else if hot { 14 } else { 7 };
        let (cx, cy) = (ir[0] + CHECK_W / 2, (ir[1] + ir[3]) / 2);
        if it.state & MF_CHECKED != 0 {
            if it.ty & MF_RADIOCHECK != 0 {
                crate::controls::fill(hdc, [cx - 2, cy - 3, cx + 3, cy + 4], mark);
                crate::controls::fill(hdc, [cx - 3, cy - 2, cx + 4, cy + 3], mark);
            } else {
                for k in 0..7 {
                    let (x, y) = (cx - 3 + k, if k < 2 { cy + k - 1 } else { cy + 2 - (k - 2) });
                    crate::controls::fill(hdc, [x, y - 1, x + 1, y + 2], mark);
                }
            }
        }
        if it.sub != 0 {
            let ax = ir[2] - ARROW_W / 2 - 2;
            for k in 0..4 {
                crate::controls::fill(hdc, [ax + k, cy - 3 + k, ax + k + 1, cy + 4 - k], mark);
            }
        }
        unsafe { crate::SetTextColor(hdc, oc) };
    }
}

pub unsafe extern "system" fn popup_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = crate::paint::BeginPaint(hwnd, ps.as_mut_ptr());
            paint_popup(hwnd, hdc);
            crate::paint::EndPaint(hwnd, ps.as_ptr());
            0
        }
        0x0014 => 1,
        _ => crate::message::def_window_proc(hwnd, msg, wp, lp, true),
    }
}

// -----| the menu loop |-----

struct Level {
    menu: u64,
    win: Hwnd,
    sel: Option<usize>,
}

struct Loop {
    owner: Hwnd,
    /// The menu bar (0 for TrackPopupMenu) and its highlighted item.
    bar: u64,
    bar_sel: Option<usize>,
    levels: Vec<Level>,
    notify: bool,
    system: bool,
    /// The item chosen (its id), or None while running / cancelled.
    chosen: Option<u32>,
    done: bool,
}

/// EndMenu was called.
static CANCEL: Lock<bool> = Lock::new(false);

fn selectable(it: &Item) -> bool {
    it.ty & MF_SEPARATOR == 0
}

/// WM_MENUSELECT for item `i` of `menu` (a popup item names its
/// position).
fn menu_select(lp: &Loop, menu: u64, i: usize) {
    let Some(it) = get_item(menu, i) else { return };
    let mut flags = (it.state & (MF_GRAYED | MF_DISABLED | MF_CHECKED)) | MF_HILITE | (it.ty & (MF_BITMAP | MF_OWNERDRAW));
    if lp.system {
        flags |= MF_SYSMENU;
    }
    let item = if it.sub != 0 {
        flags |= MF_POPUP;
        i as u64
    } else {
        it.id as u64 & 0xFFFF
    };
    send(lp.owner, WM_MENUSELECT, (flags as u64) << 16 | item, menu as i64);
}

fn set_sel(lp: &mut Loop, level: usize, sel: Option<usize>) {
    let l = &mut lp.levels[level];
    if l.sel == sel {
        return;
    }
    l.sel = sel;
    let (win, menu) = (l.win, l.menu);
    if let Some(p) = POPUPS.lock().iter_mut().find(|p| p.0 == win) {
        p.2 = sel;
    }
    crate::paint::invalidate(win, None, true);
    if let Some(i) = sel {
        menu_select(lp, menu, i);
    }
}

/// Open `menu` as a popup at screen (x, y); keyboard-opened menus
/// highlight their first item.
fn open_popup(lp: &mut Loop, menu: u64, pos: usize, x: i32, y: i32, keyboard: bool) {
    if lp.notify {
        send(lp.owner, WM_INITMENUPOPUP, menu, ((lp.system as i64) << 16) | pos as i64);
    }
    let (w, h, _) = popup_layout(menu);
    let (sw, sh) = crate::screen_size();
    let (x, y) = (x.min(sw - w).max(0), if y + h > sh { (y - h).max(0) } else { y });
    let class: Vec<u16> = "#32768\0".encode_utf16().collect();
    let win = unsafe { crate::window::CreateWindowExW(0x8, class.as_ptr(), core::ptr::null(), 0x8000_0000, x, y, w, h, lp.owner, 0, 0, 0) };
    POPUPS.lock().push((win, menu, None));
    crate::window::ShowWindow(win, 8);
    lp.levels.push(Level { menu, win, sel: None });
    if keyboard {
        let first = items_of(menu).iter().position(selectable);
        let k = lp.levels.len() - 1;
        set_sel(lp, k, first);
    }
}

/// Close the popups from level `from` on, innermost first.
fn close_from(lp: &mut Loop, from: usize) {
    while lp.levels.len() > from {
        let l = lp.levels.pop().unwrap();
        if lp.notify {
            send(lp.owner, WM_UNINITMENUPOPUP, l.menu, (lp.system as i64) << 16);
        }
        POPUPS.lock().retain(|p| p.0 != l.win);
        crate::window::DestroyWindow(l.win);
    }
}

/// Open the bar item `i`'s popup (or just highlight it).
fn bar_select(lp: &mut Loop, i: usize, open: bool, keyboard: bool) {
    close_from(lp, 0);
    lp.bar_sel = Some(i);
    *HOT_BAR.lock() = (lp.owner, Some(i));
    crate::paint::paint_frame(lp.owner);
    menu_select(lp, lp.bar, i);
    let Some(it) = get_item(lp.bar, i) else { return };
    if open && it.sub != 0 {
        let mut wr = [0i32; 4];
        crate::window::GetWindowRect(lp.owner, &mut wr);
        let r = bar_items(lp.owner)[i];
        open_popup(lp, it.sub, i, wr[0] + r[0], wr[1] + r[3], keyboard);
    }
}

/// Choose item `i` of the innermost popup (or of the bar).
fn choose(lp: &mut Loop, level: Option<usize>, i: usize, keyboard: bool) {
    let menu = match level {
        Some(l) => lp.levels[l].menu,
        None => lp.bar,
    };
    let Some(it) = get_item(menu, i) else { return };
    if it.sub != 0 {
        match level {
            None => bar_select(lp, i, true, keyboard),
            Some(l) => {
                close_from(lp, l + 1);
                set_sel(lp, l, Some(i));
                let win = lp.levels[l].win;
                let mut wr = [0i32; 4];
                crate::window::GetWindowRect(win, &mut wr);
                let (_, _, rects) = popup_layout(menu);
                open_popup(lp, it.sub, i, wr[2] - 3, wr[1] + rects[i][1] - 2, keyboard);
            }
        }
        return;
    }
    // A grayed item closes the menu without a command.
    if it.state & (MF_GRAYED | MF_DISABLED) == 0 && selectable(&it) {
        lp.chosen = Some(it.id);
    }
    lp.done = true;
}

/// Move the highlight in the innermost popup by `step` (wrapping, over
/// separators).
fn step(lp: &mut Loop, step: i32) {
    let Some(k) = lp.levels.len().checked_sub(1) else { return };
    let items = items_of(lp.levels[k].menu);
    let n = items.len() as i32;
    if n == 0 {
        return;
    }
    let mut i = match lp.levels[k].sel {
        Some(s) => s as i32,
        None if step > 0 => -1,
        None => n,
    };
    for _ in 0..n {
        i = (i + step).rem_euclid(n);
        if selectable(&items[i as usize]) {
            set_sel(lp, k, Some(i as usize));
            return;
        }
    }
}

fn key(lp: &mut Loop, vk: u32) {
    let k = lp.levels.len();
    match vk {
        0x26 => step(lp, -1),
        0x28 => {
            if k == 0 {
                if let Some(b) = lp.bar_sel {
                    bar_select(lp, b, true, true);
                }
            } else {
                step(lp, 1);
            }
        }
        0x27 => {
            // Right: into a submenu, or the next bar item.
            let sub = (k > 0).then(|| lp.levels[k - 1].sel.and_then(|s| get_item(lp.levels[k - 1].menu, s)).filter(|it| it.sub != 0)).flatten();
            if sub.is_some() {
                let s = lp.levels[k - 1].sel.unwrap();
                choose(lp, Some(k - 1), s, true);
            } else if lp.bar != 0 {
                let n = GetMenuItemCount(lp.bar).max(1) as usize;
                let b = (lp.bar_sel.unwrap_or(0) + 1) % n;
                bar_select(lp, b, k > 0, true);
            }
        }
        0x25 => {
            // Left: out of a submenu, or the previous bar item.
            if k > 1 {
                close_from(lp, k - 1);
            } else if lp.bar != 0 {
                let n = GetMenuItemCount(lp.bar).max(1) as usize;
                let b = (lp.bar_sel.unwrap_or(0) + n - 1) % n;
                bar_select(lp, b, k > 0, true);
            }
        }
        0x0D => {
            if k == 0 {
                if let Some(b) = lp.bar_sel {
                    choose(lp, None, b, true);
                }
            } else if let Some(s) = lp.levels[k - 1].sel {
                choose(lp, Some(k - 1), s, true);
            } else {
                lp.done = true;
            }
        }
        0x1B => {
            // Escape: close the innermost popup; from a bar popup back to
            // the bar; otherwise out.
            if k > 1 || (k == 1 && lp.bar != 0) {
                close_from(lp, k - 1);
            } else {
                lp.done = true;
            }
        }
        0x12 | 0x79 => lp.done = true,
        _ => {}
    }
}

/// A typed character: the item of the innermost popup (or the bar) with
/// that mnemonic.
fn mnemonic(lp: &mut Loop, ch: u16) {
    let lower = |c: u16| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c };
    let (menu, level) = match lp.levels.len() {
        0 => (lp.bar, None),
        k => (lp.levels[k - 1].menu, Some(k - 1)),
    };
    let items = items_of(menu);
    let hit = items.iter().position(|it| it.text.windows(2).any(|w| w[0] == b'&' as u16 && lower(w[1]) == lower(ch)));
    if let Some(i) = hit {
        choose(lp, level, i, true);
    }
}

/// Which popup level (or the bar: None) a mouse message's window is,
/// and the item under it.
fn hit(lp: &Loop, m: &nt_abi::Msg) -> Option<(Option<usize>, Option<usize>)> {
    if let Some(l) = lp.levels.iter().position(|l| l.win == m.hwnd) {
        let y = ((m.lparam >> 16) & 0xFFFF) as i16 as i32;
        let (_, _, rects) = popup_layout(lp.levels[l].menu);
        return Some((Some(l), rects.iter().position(|r| y >= r[1] && y < r[3])));
    }
    if lp.bar != 0 && m.hwnd == lp.owner && (0xA0..=0xAD).contains(&m.message) {
        let mut wr = [0i32; 4];
        crate::window::GetWindowRect(lp.owner, &mut wr);
        let (x, y) = (m.pt_x - wr[0], m.pt_y - wr[1]);
        let i = bar_items(lp.owner).iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]);
        return Some((None, i));
    }
    None
}

/// Run the menu loop until an item is chosen or the menu is left: the
/// chosen item's id.
fn run(lp: &mut Loop) -> Option<u32> {
    *CANCEL.lock() = false;
    let mut m = nt_abi::Msg::default();
    while !lp.done {
        if *CANCEL.lock() {
            break;
        }
        if crate::message::GetMessageW(&mut m, 0, 0, 0) == 0 {
            crate::message::PostQuitMessage(m.wparam as i32);
            break;
        }
        match m.message {
            0x0100 | 0x0104 => key(lp, m.wparam as u32),
            0x0101 | 0x0105 => {}
            0x0102 | 0x0106 => mnemonic(lp, m.wparam as u16),
            0x0201 | 0x0204 | 0x00A1 | 0x00A4 => match hit(lp, &m) {
                Some((level, Some(i))) => choose(lp, level, i, false),
                Some((_, None)) => {}
                // A click anywhere else leaves the menu (and is eaten).
                None => lp.done = true,
            },
            0x0200..=0x020E | 0x00A0..=0x00AD => {}
            _ => {
                crate::message::TranslateMessage(&m);
                crate::message::DispatchMessageW(&m);
            }
        }
    }
    close_from(lp, 0);
    if lp.bar != 0 {
        *HOT_BAR.lock() = (0, None);
        crate::paint::paint_frame(lp.owner);
    }
    send(lp.owner, WM_MENUSELECT, 0xFFFF << 16, 0);
    if lp.notify {
        send(lp.owner, WM_EXITMENULOOP, (lp.bar == 0) as u64, 0);
    }
    lp.chosen
}

const TPM_CENTERALIGN: u32 = 0x0004;
const TPM_RIGHTALIGN: u32 = 0x0008;
const TPM_VCENTERALIGN: u32 = 0x0010;
const TPM_BOTTOMALIGN: u32 = 0x0020;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

#[no_mangle]
pub extern "system" fn TrackPopupMenu(menu: u64, flags: u32, x: i32, y: i32, _reserved: i32, owner: Hwnd, _rect: *const [i32; 4]) -> i32 {
    if with_menu(menu, |_| ()).is_none() || state().get(owner).is_none() {
        return 0;
    }
    let notify = flags & TPM_NONOTIFY == 0;
    let system = is_system(owner, menu);
    let mut lp = Loop { owner, bar: 0, bar_sel: None, levels: Vec::new(), notify, system, chosen: None, done: false };
    if notify {
        send(owner, WM_ENTERMENULOOP, 1, 0);
        send(owner, WM_INITMENU, menu, 0);
    }
    let (w, h, _) = popup_layout(menu);
    let x = match flags & (TPM_CENTERALIGN | TPM_RIGHTALIGN) {
        TPM_CENTERALIGN => x - w / 2,
        TPM_RIGHTALIGN => x - w,
        _ => x,
    };
    let y = match flags & (TPM_VCENTERALIGN | TPM_BOTTOMALIGN) {
        TPM_VCENTERALIGN => y - h / 2,
        TPM_BOTTOMALIGN => y - h,
        _ => y,
    };
    open_popup(&mut lp, menu, 0, x, y, false);
    let chosen = run(&mut lp);
    if flags & TPM_RETURNCMD != 0 {
        return chosen.map_or(0, |c| c as i32);
    }
    if let Some(id) = chosen {
        let msg = if system { WM_SYSCOMMAND } else { WM_COMMAND };
        crate::message::PostMessageW(owner, msg, id as u64, 0);
        return 1;
    }
    0
}

#[no_mangle]
pub extern "system" fn TrackPopupMenuEx(menu: u64, flags: u32, x: i32, y: i32, owner: Hwnd, _params: *const u8) -> Bool {
    TrackPopupMenu(menu, flags, x, y, 0, owner, core::ptr::null())
}

#[no_mangle]
pub extern "system" fn EndMenu() -> Bool {
    *CANCEL.lock() = true;
    1
}

/// The menu bar from the mouse or keyboard: item `start` (open it if
/// `open`), with the keyboard if `keyboard`.
fn bar_loop(hwnd: Hwnd, start: usize, open: bool, keyboard: bool) {
    let bar = GetMenu(hwnd);
    if bar == 0 {
        return;
    }
    let mut lp = Loop { owner: hwnd, bar, bar_sel: None, levels: Vec::new(), notify: true, system: false, chosen: None, done: false };
    send(hwnd, WM_ENTERMENULOOP, 0, 0);
    send(hwnd, WM_INITMENU, bar, 0);
    bar_select(&mut lp, start, open, keyboard);
    if let Some(id) = run(&mut lp) {
        crate::message::PostMessageW(hwnd, WM_COMMAND, id as u64, 0);
    }
}

/// A click on the menu bar at screen (x, y).
pub fn bar_click(hwnd: Hwnd, x: i32, y: i32) {
    let mut wr = [0i32; 4];
    crate::window::GetWindowRect(hwnd, &mut wr);
    let (wx, wy) = (x - wr[0], y - wr[1]);
    if let Some(i) = bar_items(hwnd).iter().position(|r| wx >= r[0] && wx < r[2] && wy >= r[1] && wy < r[3]) {
        bar_loop(hwnd, i, true, false);
    }
}

/// WM_SYSCOMMAND SC_KEYMENU: Alt+`ch` (0: Alt or F10 alone, ' ': the
/// system menu).
pub fn key_menu(hwnd: Hwnd, ch: u16) {
    if ch == b' ' as u16 {
        let sys = GetSystemMenu(hwnd, 0);
        let mut wr = [0i32; 4];
        crate::window::GetWindowRect(hwnd, &mut wr);
        let mut lp = Loop { owner: hwnd, bar: 0, bar_sel: None, levels: Vec::new(), notify: true, system: true, chosen: None, done: false };
        send(hwnd, WM_ENTERMENULOOP, 0, 0);
        send(hwnd, WM_INITMENU, sys, 0);
        open_popup(&mut lp, sys, 0, wr[0], wr[1] + 31, true);
        if let Some(id) = run(&mut lp) {
            crate::message::PostMessageW(hwnd, WM_SYSCOMMAND, id as u64, 0);
        }
        return;
    }
    let bar = GetMenu(hwnd);
    if bar == 0 {
        return;
    }
    if ch == 0 {
        bar_loop(hwnd, 0, false, true);
        return;
    }
    let lower = |c: u16| if (b'A' as u16..=b'Z' as u16).contains(&c) { c + 32 } else { c };
    let hit = items_of(bar).iter().position(|it| it.text.windows(2).any(|w| w[0] == b'&' as u16 && lower(w[1]) == lower(ch)));
    match hit {
        Some(i) => {
            let it = get_item(bar, i).unwrap();
            if it.sub != 0 {
                bar_loop(hwnd, i, true, true);
            } else if it.state & (MF_GRAYED | MF_DISABLED) == 0 {
                crate::message::PostMessageW(hwnd, WM_COMMAND, it.id as u64, 0);
            }
        }
        None => {
            crate::paint::MessageBeep(0);
        }
    }
}

// -----| the system menu |-----

/// Each window's system menu copy.
static SYSTEM: Lock<Vec<(Hwnd, u64)>> = Lock::new(Vec::new());

fn is_system(hwnd: Hwnd, menu: u64) -> bool {
    SYSTEM.lock().iter().any(|e| e.0 == hwnd && e.1 == menu)
}

fn make_system_menu() -> u64 {
    let m = new_menu();
    let items: [(u32, &str, u32); 7] = [
        (0xF120, "&Restore", MF_GRAYED),
        (0xF010, "&Move", 0),
        (0xF000, "&Size", 0),
        (0xF020, "Mi&nimize", 0),
        (0xF030, "Ma&ximize", 0),
        (0, "", MF_SEPARATOR),
        (0xF060, "&Close\tAlt+F4", MF_DEFAULT),
    ];
    for (id, text, flags) in items {
        insert(m, None, make_item(flags, id as u64, text.encode_utf16().collect()));
    }
    m
}

#[no_mangle]
pub extern "system" fn GetSystemMenu(hwnd: Hwnd, revert: Bool) -> u64 {
    let mut sys = SYSTEM.lock();
    let have = sys.iter().position(|e| e.0 == hwnd);
    if revert != 0 {
        if let Some(i) = have {
            let (_, m) = sys.remove(i);
            drop(sys);
            DestroyMenu(m);
        }
        return 0;
    }
    match have {
        Some(i) => sys[i].1,
        None => {
            drop(sys);
            let m = make_system_menu();
            SYSTEM.lock().push((hwnd, m));
            m
        }
    }
}

/// A window is gone: its system menu too.
pub fn window_destroyed(hwnd: Hwnd) {
    let m = {
        let mut sys = SYSTEM.lock();
        let i = sys.iter().position(|e| e.0 == hwnd);
        i.map(|i| sys.remove(i).1)
    };
    if let Some(m) = m {
        DestroyMenu(m);
    }
}

// -----| accelerators |-----

const FVIRTKEY: u8 = 0x01;
const FSHIFT: u8 = 0x04;
const FCONTROL: u8 = 0x08;
const FALT: u8 = 0x10;

static ACCELS: Lock<Vec<Option<Vec<(u8, u16, u16)>>>> = Lock::new(Vec::new());
const ACCEL_BASE: u64 = 0x0006_0000;

fn new_accel(entries: Vec<(u8, u16, u16)>) -> u64 {
    let mut v = ACCELS.lock();
    let i = match v.iter().position(|e| e.is_none()) {
        Some(i) => i,
        None => {
            v.push(None);
            v.len() - 1
        }
    };
    v[i] = Some(entries);
    ACCEL_BASE + 4 * i as u64
}

fn accel(h: u64) -> Option<Vec<(u8, u16, u16)>> {
    let i = (h.checked_sub(ACCEL_BASE)? / 4) as usize;
    ACCELS.lock().get(i)?.clone()
}

/// ACCEL: fVirt (byte), key, cmd (words).
#[no_mangle]
pub unsafe extern "system" fn CreateAcceleratorTableW(a: *const u8, n: i32) -> u64 {
    if a.is_null() || n <= 0 {
        return 0;
    }
    let e = (0..n as usize)
        .map(|i| unsafe {
            let p = a.add(i * 6);
            (*p, core::ptr::read_unaligned(p.add(2) as *const u16), core::ptr::read_unaligned(p.add(4) as *const u16))
        })
        .collect();
    new_accel(e)
}

#[no_mangle]
pub unsafe extern "system" fn CreateAcceleratorTableA(a: *const u8, n: i32) -> u64 {
    unsafe { CreateAcceleratorTableW(a, n) }
}

#[no_mangle]
pub extern "system" fn DestroyAcceleratorTable(h: u64) -> Bool {
    let Some(i) = h.checked_sub(ACCEL_BASE).map(|d| (d / 4) as usize) else { return 0 };
    ACCELS.lock().get_mut(i).and_then(|e| e.take()).is_some() as Bool
}

#[no_mangle]
pub unsafe extern "system" fn CopyAcceleratorTableW(h: u64, out: *mut u8, n: i32) -> i32 {
    let Some(e) = accel(h) else { return 0 };
    if out.is_null() {
        return e.len() as i32;
    }
    let k = e.len().min(n.max(0) as usize);
    for (i, (v, key, cmd)) in e.iter().take(k).enumerate() {
        unsafe {
            let p = out.add(i * 6);
            *p = *v;
            *p.add(1) = 0;
            core::ptr::write_unaligned(p.add(2) as *mut u16, *key);
            core::ptr::write_unaligned(p.add(4) as *mut u16, *cmd);
        }
    }
    k as i32
}

#[no_mangle]
pub unsafe extern "system" fn CopyAcceleratorTableA(h: u64, out: *mut u8, n: i32) -> i32 {
    unsafe { CopyAcceleratorTableW(h, out, n) }
}

/// An RT_ACCELERATOR resource: entries of fFlags, key, id, padding; the
/// last has 0x80 in its flags.
#[no_mangle]
pub extern "system" fn LoadAcceleratorsW(inst: u64, name: *const u16) -> u64 {
    let Some(d) = resource(inst, name, 9) else { return 0 };
    let mut e = Vec::new();
    for c in d.chunks_exact(8) {
        let flags = u16::from_le_bytes([c[0], c[1]]);
        e.push(((flags & 0x7F) as u8, u16::from_le_bytes([c[2], c[3]]), u16::from_le_bytes([c[4], c[5]])));
        if flags & 0x80 != 0 {
            break;
        }
    }
    new_accel(e)
}

#[no_mangle]
pub unsafe extern "system" fn LoadAcceleratorsA(inst: u64, name: *const u8) -> u64 {
    let (_k, w) = unsafe { name_a(name) };
    LoadAcceleratorsW(inst, w)
}

/// The top-level popup of `bar` holding command `id` (its position), or
/// None if it's a bar item itself.
fn popup_holding(bar: u64, id: u32) -> Option<Option<usize>> {
    let items = items_of(bar);
    if items.iter().any(|it| it.sub == 0 && it.id == id) {
        return Some(None);
    }
    items.iter().position(|it| it.sub != 0 && find(it.sub, id, 0).is_some()).map(Some)
}

fn translate(hwnd: Hwnd, h: u64, m: *const nt_abi::Msg) -> i32 {
    let Some(table) = accel(h) else { return 0 };
    if m.is_null() {
        return 0;
    }
    let m = unsafe { *m };
    let (virt, ch) = match m.message {
        0x0100 | 0x0104 => (true, false),
        0x0102 | 0x0106 => (false, true),
        _ => return 0,
    };
    let down = |vk| crate::keyboard::GetKeyState(vk) < 0;
    let (shift, ctrl, alt) = (down(0x10), down(0x11), down(0x12) || m.message == 0x0104 || m.message == 0x0106);
    let hit = table.iter().find(|(f, key, _)| {
        if virt && f & FVIRTKEY != 0 {
            *key as u64 == m.wparam && (f & FSHIFT != 0) == shift && (f & FCONTROL != 0) == ctrl && (f & FALT != 0) == alt
        } else if ch && f & FVIRTKEY == 0 {
            *key as u64 == m.wparam && (f & FALT != 0) == alt
        } else {
            false
        }
    });
    let Some(&(_, _, cmd)) = hit else { return 0 };
    let cmd = cmd as u32;
    // With the command in the window's menu: WM_INITMENU (and the popup
    // holding it), and no WM_COMMAND if it's grayed.
    let bar = GetMenu(hwnd);
    let holder = if bar != 0 { popup_holding(bar, cmd) } else { None };
    let mut enabled = true;
    if let Some(pos) = holder {
        send(hwnd, WM_INITMENU, bar, 0);
        if let Some(p) = pos {
            send(hwnd, WM_INITMENUPOPUP, GetSubMenu(bar, p as i32), p as i64);
        }
        enabled = GetMenuState(bar, cmd, 0) & (MF_GRAYED | MF_DISABLED) == 0;
        if enabled {
            send(hwnd, WM_COMMAND, 1 << 16 | cmd as u64, 0);
        }
        if let Some(p) = pos {
            send(hwnd, WM_UNINITMENUPOPUP, GetSubMenu(bar, p as i32), 0);
        }
        return 1;
    }
    if enabled {
        send(hwnd, WM_COMMAND, 1 << 16 | cmd as u64, 0);
    }
    1
}

#[no_mangle]
pub extern "system" fn TranslateAcceleratorW(hwnd: Hwnd, h: u64, m: *const nt_abi::Msg) -> i32 {
    translate(hwnd, h, m)
}

#[no_mangle]
pub extern "system" fn TranslateAcceleratorA(hwnd: Hwnd, h: u64, m: *const nt_abi::Msg) -> i32 {
    translate(hwnd, h, m)
}
