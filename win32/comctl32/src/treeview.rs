//! The tree view ("SysTreeView32"): a hierarchy of items with lines, +/-
//! buttons, images and checkboxes; items may leave their text, images and
//! child count to the parent. Lazy filling through TVN_ITEMEXPANDING, the
//! selection rules, keyboard, sorting and label editing go as Windows has
//! them. Not yet: dragging (the desktop reports clicks, not movement) and
//! custom draw.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const TVM_FIRST: u32 = 0x1100;
const TVM_INSERTITEMA: u32 = TVM_FIRST;
const TVM_DELETEITEM: u32 = TVM_FIRST + 1;
const TVM_EXPAND: u32 = TVM_FIRST + 2;
const TVM_GETITEMRECT: u32 = TVM_FIRST + 4;
const TVM_GETCOUNT: u32 = TVM_FIRST + 5;
const TVM_GETINDENT: u32 = TVM_FIRST + 6;
const TVM_SETINDENT: u32 = TVM_FIRST + 7;
const TVM_GETIMAGELIST: u32 = TVM_FIRST + 8;
const TVM_SETIMAGELIST: u32 = TVM_FIRST + 9;
const TVM_GETNEXTITEM: u32 = TVM_FIRST + 10;
const TVM_SELECTITEM: u32 = TVM_FIRST + 11;
const TVM_GETITEMA: u32 = TVM_FIRST + 12;
const TVM_SETITEMA: u32 = TVM_FIRST + 13;
const TVM_EDITLABELA: u32 = TVM_FIRST + 14;
const TVM_GETEDITCONTROL: u32 = TVM_FIRST + 15;
const TVM_GETVISIBLECOUNT: u32 = TVM_FIRST + 16;
const TVM_HITTEST: u32 = TVM_FIRST + 17;
const TVM_SORTCHILDREN: u32 = TVM_FIRST + 19;
const TVM_ENSUREVISIBLE: u32 = TVM_FIRST + 20;
const TVM_SORTCHILDRENCB: u32 = TVM_FIRST + 21;
const TVM_ENDEDITLABELNOW: u32 = TVM_FIRST + 22;
const TVM_SETITEMHEIGHT: u32 = TVM_FIRST + 27;
const TVM_GETITEMHEIGHT: u32 = TVM_FIRST + 28;
const TVM_SETBKCOLOR: u32 = TVM_FIRST + 29;
const TVM_SETTEXTCOLOR: u32 = TVM_FIRST + 30;
const TVM_GETBKCOLOR: u32 = TVM_FIRST + 31;
const TVM_GETTEXTCOLOR: u32 = TVM_FIRST + 32;
const TVM_GETITEMSTATE: u32 = TVM_FIRST + 39;
const TVM_SETLINECOLOR: u32 = TVM_FIRST + 40;
const TVM_GETLINECOLOR: u32 = TVM_FIRST + 41;
const TVM_SETEXTENDEDSTYLE: u32 = TVM_FIRST + 44;
const TVM_GETEXTENDEDSTYLE: u32 = TVM_FIRST + 45;
const TVM_INSERTITEMW: u32 = TVM_FIRST + 50;
const TVM_GETITEMW: u32 = TVM_FIRST + 62;
const TVM_SETITEMW: u32 = TVM_FIRST + 63;
const TVM_EDITLABELW: u32 = TVM_FIRST + 65;

const TVS_HASBUTTONS: u32 = 0x1;
const TVS_HASLINES: u32 = 0x2;
const TVS_LINESATROOT: u32 = 0x4;
const TVS_EDITLABELS: u32 = 0x8;
const TVS_SHOWSELALWAYS: u32 = 0x20;
const TVS_CHECKBOXES: u32 = 0x100;
const TVS_SINGLEEXPAND: u32 = 0x400;
const TVS_FULLROWSELECT: u32 = 0x1000;

const TVIF_TEXT: u32 = 0x1;
const TVIF_IMAGE: u32 = 0x2;
const TVIF_PARAM: u32 = 0x4;
const TVIF_STATE: u32 = 0x8;
const TVIF_SELECTEDIMAGE: u32 = 0x20;
const TVIF_CHILDREN: u32 = 0x40;

const TVIS_SELECTED: u32 = 0x2;
const TVIS_BOLD: u32 = 0x10;
const TVIS_EXPANDED: u32 = 0x20;
const TVIS_EXPANDEDONCE: u32 = 0x40;
const TVIS_STATEIMAGEMASK: u32 = 0xF000;

const TVI_ROOT: u64 = 0xFFFF_FFFF_FFFF_0000;
const TVI_FIRST: u64 = 0xFFFF_FFFF_FFFF_0001;
const TVI_LAST: u64 = 0xFFFF_FFFF_FFFF_0002;
const TVI_SORT: u64 = 0xFFFF_FFFF_FFFF_0003;

const TVGN_ROOT: u32 = 0;
const TVGN_NEXT: u32 = 1;
const TVGN_PREVIOUS: u32 = 2;
const TVGN_PARENT: u32 = 3;
const TVGN_CHILD: u32 = 4;
const TVGN_FIRSTVISIBLE: u32 = 5;
const TVGN_NEXTVISIBLE: u32 = 6;
const TVGN_PREVIOUSVISIBLE: u32 = 7;
const TVGN_DROPHILITE: u32 = 8;
const TVGN_CARET: u32 = 9;
const TVGN_LASTVISIBLE: u32 = 10;

const TVE_COLLAPSE: u32 = 1;
const TVE_EXPAND: u32 = 2;
const TVE_TOGGLE: u32 = 3;
const TVE_COLLAPSERESET: u32 = 0x8000;

const TVC_UNKNOWN: u32 = 0;
const TVC_BYMOUSE: u32 = 1;
const TVC_BYKEYBOARD: u32 = 2;

const TVHT_NOWHERE: u32 = 1;
const TVHT_ONITEMICON: u32 = 2;
const TVHT_ONITEMLABEL: u32 = 4;
const TVHT_ONITEMINDENT: u32 = 8;
const TVHT_ONITEMBUTTON: u32 = 0x10;
const TVHT_ONITEMRIGHT: u32 = 0x20;
const TVHT_ONITEMSTATEICON: u32 = 0x40;

const TVN_SELCHANGINGA: i32 = -401;
const TVN_SELCHANGEDA: i32 = -402;
const TVN_GETDISPINFOA: i32 = -403;
const TVN_ITEMEXPANDINGA: i32 = -405;
const TVN_ITEMEXPANDEDA: i32 = -406;
const TVN_DELETEITEMA: i32 = -409;
const TVN_BEGINLABELEDITA: i32 = -410;
const TVN_ENDLABELEDITA: i32 = -411;
const TVN_KEYDOWN: i32 = -412;
const TVN_SELCHANGINGW: i32 = -450;
const TVN_SELCHANGEDW: i32 = -451;
const TVN_GETDISPINFOW: i32 = -452;
const TVN_ITEMEXPANDINGW: i32 = -454;
const TVN_ITEMEXPANDEDW: i32 = -455;
const TVN_DELETEITEMW: i32 = -458;
const TVN_BEGINLABELEDITW: i32 = -459;
const TVN_ENDLABELEDITW: i32 = -460;

const NM_CLICK: i32 = -2;
const NM_DBLCLK: i32 = -3;
const NM_RETURN: i32 = -4;
const NM_RCLICK: i32 = -5;
const NM_SETFOCUS: i32 = -7;
const NM_KILLFOCUS: i32 = -8;

const TEXT_CALLBACK: u64 = u64::MAX;
const IMAGE_CALLBACK: i32 = -1;
const CHILDREN_CALLBACK: i32 = -1;

#[derive(Clone)]
struct Node {
    parent: u64,
    children: Vec<u64>,
    /// None: asked of the parent.
    text: Option<Vec<u16>>,
    image: i32,
    sel_image: i32,
    state: u32,
    /// cChildren when set: 0, 1, or I_CHILDRENCALLBACK; else from `children`.
    has_children: Option<i32>,
    param: i64,
}

struct Tree {
    nodes: BTreeMap<u64, Node>,
    roots: Vec<u64>,
    next: u64,
    selected: u64,
    drop_hilite: u64,
    /// The first visible row, and how far it's scrolled across.
    top: i32,
    sx: i32,
    font: u64,
    images: u64,
    state_images: u64,
    indent: i32,
    item_h: i32,
    bk: i32,
    text_color: i32,
    line_color: u32,
    ex: u32,
    edit: Hwnd,
    edit_item: u64,
    has_focus: bool,
}

static TREES: Lock<BTreeMap<Hwnd, Tree>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut Tree) -> R) -> Option<R> {
    TREES.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("SysTreeView32", tree_proc as *const () as usize, true);
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

fn client(hwnd: Hwnd) -> [i32; 4] {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    r
}

fn invalidate(hwnd: Hwnd) {
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

/// A row's height: what was set, else the font's or the images'.
fn item_height(hwnd: Hwnd) -> i32 {
    let (set, font, images) = with(hwnd, |t| (t.item_h, t.font, t.images)).unwrap_or((0, 0, 0));
    if set > 0 {
        return set;
    }
    let mut h = crate::font_height(hwnd, font) + 3;
    if images != 0 {
        let (mut cx, mut cy) = (0, 0);
        unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
        h = h.max(cy + 1);
    }
    h
}

/// Every visible item with its depth, top to bottom.
fn visible(hwnd: Hwnd) -> Vec<(u64, i32)> {
    with(hwnd, |t| {
        let mut out = Vec::new();
        let mut stack: Vec<(u64, i32)> = t.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            out.push((id, depth));
            if let Some(n) = t.nodes.get(&id) {
                if n.state & TVIS_EXPANDED != 0 {
                    stack.extend(n.children.iter().rev().map(|&c| (c, depth + 1)));
                }
            }
        }
        out
    })
    .unwrap_or_default()
}

fn node<R>(hwnd: Hwnd, id: u64, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
    with(hwnd, |t| t.nodes.get_mut(&id).map(f)).flatten()
}

// -----| notifications |-----

/// TVITEM: mask, handle, state, stateMask, text, cap, images, children, param.
fn tvitem(mask: u32, id: u64, state: u32, text: u64, cap: i32, image: i32, sel: i32, children: i32, param: i64) -> [u8; 56] {
    let mut b = [0u8; 56];
    b[0..4].copy_from_slice(&mask.to_le_bytes());
    b[8..16].copy_from_slice(&id.to_le_bytes());
    b[16..20].copy_from_slice(&state.to_le_bytes());
    b[24..32].copy_from_slice(&text.to_le_bytes());
    b[32..36].copy_from_slice(&cap.to_le_bytes());
    b[36..40].copy_from_slice(&image.to_le_bytes());
    b[40..44].copy_from_slice(&sel.to_le_bytes());
    b[44..48].copy_from_slice(&children.to_le_bytes());
    b[48..56].copy_from_slice(&param.to_le_bytes());
    b
}

fn summary(hwnd: Hwnd, id: u64) -> [u8; 56] {
    let (state, image, sel, param) = node(hwnd, id, |n| (n.state, n.image, n.sel_image, n.param)).unwrap_or((0, 0, 0, 0));
    // TVIF_HANDLE | TVIF_PARAM | TVIF_STATE (as NMTREEVIEW's items carry)
    tvitem(0x10 | TVIF_PARAM | TVIF_STATE, id, state, 0, 0, image, sel, 0, param)
}

/// NMTREEVIEW after the NMHDR.
fn nmtreeview(hwnd: Hwnd, code_w: i32, code_a: i32, action: u32, old: u64, new: u64) -> i64 {
    let code = if crate::parent_unicode(hwnd) { code_w } else { code_a };
    let mut e = [0u8; 128];
    e[0..4].copy_from_slice(&action.to_le_bytes());
    if old != 0 {
        e[8..64].copy_from_slice(&summary(hwnd, old));
    }
    if new != 0 {
        e[64..120].copy_from_slice(&summary(hwnd, new));
    }
    crate::notify(hwnd, code, &e)
}

/// TVN_GETDISPINFO: the answers for what the item leaves to the parent.
fn dispinfo(hwnd: Hwnd, id: u64, mask: u32) -> (Vec<u16>, i32, i32, i32) {
    let unicode = crate::parent_unicode(hwnd);
    let (state, param) = node(hwnd, id, |n| (n.state, n.param)).unwrap_or((0, 0));
    let mut buf = alloc::vec![0u16; 260];
    let cap = if unicode { 260 } else { 520 };
    let mut msg = [0u8; 24 + 56];
    let idc = unsafe { crate::GetDlgCtrlID(hwnd) } as u64;
    msg[0..8].copy_from_slice(&hwnd.to_le_bytes());
    msg[8..16].copy_from_slice(&idc.to_le_bytes());
    let code = if unicode { TVN_GETDISPINFOW } else { TVN_GETDISPINFOA };
    msg[16..20].copy_from_slice(&(code as u32).to_le_bytes());
    msg[24..80].copy_from_slice(&tvitem(mask | 0x10, id, state, buf.as_mut_ptr() as u64, cap, 0, 0, 0, param));
    let parent = unsafe { crate::GetParent(hwnd) };
    unsafe { crate::SendMessageW(parent, crate::WM_NOTIFY, idc, msg.as_mut_ptr() as i64) };
    let text_ptr = u64::from_le_bytes(msg[48..56].try_into().unwrap());
    let image = i32::from_le_bytes(msg[60..64].try_into().unwrap());
    let sel = i32::from_le_bytes(msg[64..68].try_into().unwrap());
    let children = i32::from_le_bytes(msg[68..72].try_into().unwrap());
    let text = if mask & TVIF_TEXT != 0 && text_ptr != 0 && text_ptr != TEXT_CALLBACK { crate::text_arg(text_ptr, unicode) } else { Vec::new() };
    (text, image, sel, children)
}

fn node_text(hwnd: Hwnd, id: u64) -> Vec<u16> {
    match node(hwnd, id, |n| n.text.clone()) {
        Some(Some(t)) => t,
        Some(None) => dispinfo(hwnd, id, TVIF_TEXT).0,
        None => Vec::new(),
    }
}

fn node_images(hwnd: Hwnd, id: u64) -> (i32, i32) {
    let (image, sel) = node(hwnd, id, |n| (n.image, n.sel_image)).unwrap_or((0, 0));
    let mut mask = 0;
    if image == IMAGE_CALLBACK {
        mask |= TVIF_IMAGE;
    }
    if sel == IMAGE_CALLBACK {
        mask |= TVIF_SELECTEDIMAGE;
    }
    if mask == 0 {
        return (image, sel);
    }
    let (_, i, s, _) = dispinfo(hwnd, id, mask);
    (if image == IMAGE_CALLBACK { i } else { image }, if sel == IMAGE_CALLBACK { s } else { sel })
}

/// Whether the item shows a button: it has children, or says it does.
fn shows_children(hwnd: Hwnd, id: u64) -> bool {
    let Some((set, n)) = node(hwnd, id, |n| (n.has_children, n.children.len())) else { return false };
    match set {
        Some(CHILDREN_CALLBACK) => dispinfo(hwnd, id, TVIF_CHILDREN).3 != 0,
        Some(c) => c != 0,
        None => n > 0,
    }
}

// -----| structure |-----

fn siblings_of(t: &mut Tree, parent: u64) -> Option<&mut Vec<u64>> {
    if parent == 0 {
        Some(&mut t.roots)
    } else {
        t.nodes.get_mut(&parent).map(|n| &mut n.children)
    }
}

fn insert(hwnd: Hwnd, lp: i64, unicode: bool) -> i64 {
    if lp == 0 {
        return 0;
    }
    let p = lp as *const u8;
    let (parent, after, mask, state, state_mask, text, image, sel, children, param) = unsafe {
        let q = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u64);
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const i32);
        (q(0), q(8), d(16) as u32, d(32) as u32, d(36) as u32, q(40), d(52), d(56), d(60), q(64) as i64)
    };
    let parent = if parent == TVI_ROOT { 0 } else { parent };
    let sorted_key: Option<Vec<u16>> = (after == TVI_SORT && mask & TVIF_TEXT != 0 && text != TEXT_CALLBACK).then(|| crate::text_arg(text, unicode));
    let n = Node {
        parent,
        children: Vec::new(),
        text: if mask & TVIF_TEXT != 0 {
            if text == TEXT_CALLBACK { None } else { Some(crate::text_arg(text, unicode)) }
        } else {
            Some(Vec::new())
        },
        image: if mask & TVIF_IMAGE != 0 { image } else { 0 },
        sel_image: if mask & TVIF_SELECTEDIMAGE != 0 { sel } else { 0 },
        state: if mask & TVIF_STATE != 0 { state & state_mask & !TVIS_SELECTED } else { 0 },
        has_children: (mask & TVIF_CHILDREN != 0).then_some(children),
        param: if mask & TVIF_PARAM != 0 { param } else { 0 },
    };
    let id = with(hwnd, |t| {
        if parent != 0 && !t.nodes.contains_key(&parent) {
            return 0;
        }
        // Where among its siblings.
        let at = {
            let nodes = &t.nodes;
            let sibs = if parent == 0 { &t.roots } else { &nodes[&parent].children };
            match after {
                TVI_FIRST => 0,
                TVI_LAST => sibs.len(),
                TVI_SORT => match &sorted_key {
                    Some(k) => {
                        let lk: Vec<u16> = k.iter().map(|&c| crate::lower(c)).collect();
                        sibs.iter()
                            .position(|s| match nodes.get(s).and_then(|o| o.text.as_ref()) {
                                Some(o) => o.iter().map(|&c| crate::lower(c)).collect::<Vec<u16>>() > lk,
                                None => false,
                            })
                            .unwrap_or(sibs.len())
                    }
                    None => sibs.len(),
                },
                h => sibs.iter().position(|&s| s == h).map_or(sibs.len(), |i| i + 1),
            }
        };
        let id = t.next;
        t.next += 8;
        t.nodes.insert(id, n);
        if let Some(sibs) = siblings_of(t, parent) {
            sibs.insert(at, id);
        }
        id
    })
    .unwrap_or(0);
    if id != 0 {
        layout(hwnd);
        invalidate(hwnd);
    }
    id as i64
}

/// Items under `id`, deepest first, then `id`.
fn subtree(t: &Tree, id: u64, out: &mut Vec<u64>) {
    if let Some(n) = t.nodes.get(&id) {
        for &c in &n.children {
            subtree(t, c, out);
        }
    }
    out.push(id);
}

fn delete(hwnd: Hwnd, target: u64) -> bool {
    // Everything: one root at a time.
    if target == 0 || target == TVI_ROOT {
        for r in with(hwnd, |t| t.roots.clone()).unwrap_or_default() {
            delete(hwnd, r);
        }
        return true;
    }
    let list: Vec<u64> = with(hwnd, |t| {
        let mut out = Vec::new();
        if t.nodes.contains_key(&target) {
            subtree(t, target, &mut out);
        }
        out
    })
    .unwrap_or_default();
    if list.is_empty() {
        return false;
    }
    // Where the selection goes if it's in there: the next sibling, the one
    // before, or the parent.
    let next = with(hwnd, |t| {
        let parent = t.nodes.get(&target).map_or(0, |n| n.parent);
        let sibs = if parent == 0 { &t.roots } else { &t.nodes[&parent].children };
        let at = sibs.iter().position(|&s| s == target).unwrap_or(0);
        sibs.get(at + 1).copied().or(if at > 0 { Some(sibs[at - 1]) } else { None }).unwrap_or(parent)
    })
    .unwrap_or(0);
    for &id in &list {
        nmtreeview(hwnd, TVN_DELETEITEMW, TVN_DELETEITEMA, 0, id, 0);
    }
    let lost_selection = with(hwnd, |t| {
        let lost = list.contains(&t.selected);
        if lost {
            t.selected = 0;
        }
        for id in &list {
            t.nodes.remove(id);
        }
        t.roots.retain(|r| *r != target);
        for n in t.nodes.values_mut() {
            n.children.retain(|c| *c != target);
        }
        lost
    })
    .unwrap_or(false);
    if lost_selection && next != 0 {
        select(hwnd, next, TVC_UNKNOWN);
    }
    layout(hwnd);
    invalidate(hwnd);
    true
}

/// TVM_EXPAND and the user's clicks. `user`: always notifies; a message only
/// the first time (TVIS_EXPANDEDONCE).
fn expand(hwnd: Hwnd, id: u64, action: u32, user: bool) -> bool {
    let Some(state) = node(hwnd, id, |n| n.state) else { return false };
    let open = state & TVIS_EXPANDED != 0;
    let want_open = match action & 0xFF {
        TVE_COLLAPSE => false,
        TVE_EXPAND => true,
        TVE_TOGGLE => !open,
        _ => return false,
    };
    if want_open == open {
        return !user;
    }
    if !user && !shows_children(hwnd, id) {
        return false;
    }
    let notifying = user || state & TVIS_EXPANDEDONCE == 0;
    let verb = if want_open { TVE_EXPAND } else { TVE_COLLAPSE };
    if notifying && nmtreeview(hwnd, TVN_ITEMEXPANDINGW, TVN_ITEMEXPANDINGA, verb, 0, id) != 0 {
        return false;
    }
    node(hwnd, id, |n| {
        if want_open {
            n.state |= TVIS_EXPANDED | TVIS_EXPANDEDONCE;
        } else {
            n.state &= !TVIS_EXPANDED;
        }
    });
    // COLLAPSERESET drops the children and the "once" flag.
    if action & TVE_COLLAPSERESET != 0 && !want_open {
        let kids = node(hwnd, id, |n| n.children.clone()).unwrap_or_default();
        for k in kids {
            delete(hwnd, k);
        }
        node(hwnd, id, |n| n.state &= !TVIS_EXPANDEDONCE);
    }
    if notifying {
        nmtreeview(hwnd, TVN_ITEMEXPANDEDW, TVN_ITEMEXPANDEDA, verb, 0, id);
    }
    // A hidden selection moves up to the item that hid it.
    if !want_open {
        let sel = with(hwnd, |t| t.selected).unwrap_or(0);
        let mut p = node(hwnd, sel, |n| n.parent).unwrap_or(0);
        while p != 0 {
            if p == id {
                if user {
                    select(hwnd, id, TVC_BYMOUSE);
                } else {
                    node(hwnd, sel, |n| n.state &= !TVIS_SELECTED);
                    node(hwnd, id, |n| n.state |= TVIS_SELECTED);
                    with(hwnd, |t| t.selected = id);
                }
                break;
            }
            p = node(hwnd, p, |n| n.parent).unwrap_or(0);
        }
    }
    layout(hwnd);
    invalidate(hwnd);
    true
}

/// Select `id` (0: nothing): TVN_SELCHANGING may refuse, TVN_SELCHANGED after.
fn select(hwnd: Hwnd, id: u64, action: u32) -> bool {
    let old = with(hwnd, |t| t.selected).unwrap_or(0);
    if id == old {
        return true;
    }
    if nmtreeview(hwnd, TVN_SELCHANGINGW, TVN_SELCHANGINGA, action, old, id) != 0 {
        return false;
    }
    node(hwnd, old, |n| n.state &= !TVIS_SELECTED);
    node(hwnd, id, |n| n.state |= TVIS_SELECTED);
    with(hwnd, |t| t.selected = id);
    if id != 0 {
        ensure_visible(hwnd, id);
    }
    nmtreeview(hwnd, TVN_SELCHANGEDW, TVN_SELCHANGEDA, action, old, id);
    invalidate(hwnd);
    true
}

/// Open the ancestors and scroll to `id`.
fn ensure_visible(hwnd: Hwnd, id: u64) {
    let mut p = node(hwnd, id, |n| n.parent).unwrap_or(0);
    let mut chain = Vec::new();
    while p != 0 {
        chain.push(p);
        p = node(hwnd, p, |n| n.parent).unwrap_or(0);
    }
    for a in chain.into_iter().rev() {
        expand(hwnd, a, TVE_EXPAND, false);
    }
    let vis = visible(hwnd);
    let Some(row) = vis.iter().position(|v| v.0 == id) else { return };
    let row = row as i32;
    let rows = (client(hwnd)[3] / item_height(hwnd).max(1)).max(1);
    with(hwnd, |t| {
        if row < t.top {
            t.top = row;
        } else if row >= t.top + rows {
            t.top = row - rows + 1;
        }
    });
    layout(hwnd);
    invalidate(hwnd);
}

fn layout(hwnd: Hwnd) {
    let c = client(hwnd);
    let vis = visible(hwnd);
    let rows = (c[3] / item_height(hwnd).max(1)).max(1);
    let total = vis.len() as i32;
    let top = with(hwnd, |t| {
        t.top = t.top.clamp(0, (total - rows).max(0));
        t.top
    })
    .unwrap_or(0);
    crate::set_scroll(hwnd, 1, total, rows, top);
    // Across: the widest visible row.
    let mut widest = 0;
    if style(hwnd) & 0x8000 == 0 {
        let font = with(hwnd, |t| t.font).unwrap_or(0);
        let dc = unsafe { crate::GetDC(hwnd) };
        let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
        for (id, depth) in vis.iter().take(1024) {
            let x = label_x(hwnd, *depth);
            let t = node(hwnd, *id, |n| n.text.clone()).flatten().unwrap_or_default();
            widest = widest.max(x + crate::text_size(dc, &t).0 + 6);
        }
        unsafe {
            crate::SelectObject(dc, old);
            crate::ReleaseDC(hwnd, dc);
        }
    }
    let sx = with(hwnd, |t| {
        t.sx = t.sx.clamp(0, (widest - c[2]).max(0));
        t.sx
    })
    .unwrap_or(0);
    crate::set_scroll(hwnd, 0, widest, c[2], sx);
}

// -----| geometry and painting |-----

fn root_offset(hwnd: Hwnd) -> i32 {
    if style(hwnd) & (TVS_LINESATROOT) != 0 { 1 } else { 0 }
}

/// Where a row's button or line column is, before scrolling.
fn indent_x(hwnd: Hwnd, depth: i32) -> i32 {
    let indent = with(hwnd, |t| t.indent).unwrap_or(19);
    (depth + root_offset(hwnd) - 1) * indent
}

fn image_x(hwnd: Hwnd, depth: i32) -> i32 {
    let indent = with(hwnd, |t| t.indent).unwrap_or(19);
    (depth + root_offset(hwnd)) * indent + 3
}

fn label_x(hwnd: Hwnd, depth: i32) -> i32 {
    let (images, states) = with(hwnd, |t| (t.images, t.state_images)).unwrap_or((0, 0));
    let mut x = image_x(hwnd, depth);
    if style(hwnd) & TVS_CHECKBOXES != 0 || states != 0 {
        x += 16;
    }
    if images != 0 {
        let (mut cx, mut cy) = (0, 0);
        unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
        x += cx + 2;
    }
    x
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let c = client(hwnd);
    let Some((font, images, states, bk, text_color, line_color, has_focus, top, sx, indent)) =
        with(hwnd, |t| (t.font, t.images, t.state_images, t.bk, t.text_color, t.line_color, t.has_focus, t.top, t.sx, t.indent))
    else {
        return;
    };
    unsafe {
        if bk == -1 {
            crate::FillRect(hdc, &c, crate::GetSysColorBrush(5));
        } else {
            let b = crate::CreateSolidBrush(bk as u32);
            crate::FillRect(hdc, &c, b);
            crate::DeleteObject(b);
        }
    }
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    let st = style(hwnd);
    let h = item_height(hwnd);
    let vis = visible(hwnd);
    let line = |r: [i32; 4]| unsafe {
        // Dotted, every other pixel.
        let b = if line_color == 0xFF00_0000 { crate::GetSysColorBrush(16) } else { crate::CreateSolidBrush(line_color) };
        let (mut x, mut y) = (r[0], r[1]);
        while x < r[2] && y < r[3] {
            crate::FillRect(hdc, &[x, y, x + 1, y + 1], b);
            if r[2] - r[0] > 1 {
                x += 2;
            } else {
                y += 2;
            }
        }
        if line_color != 0xFF00_0000 {
            crate::DeleteObject(b);
        }
    };
    // For each visible row: what its lines need.
    let last_child = |id: u64| {
        with(hwnd, |t| {
            let parent = t.nodes.get(&id).map_or(0, |n| n.parent);
            let sibs = if parent == 0 { &t.roots } else { &t.nodes[&parent].children };
            sibs.last() == Some(&id)
        })
        .unwrap_or(true)
    };
    for (row, (id, depth)) in vis.iter().copied().enumerate() {
        let row = row as i32 - top;
        if row < 0 {
            continue;
        }
        let y = row * h;
        if y >= c[3] {
            break;
        }
        let mid = y + h / 2;
        let bx = indent_x(hwnd, depth) + indent / 2 - sx;
        if st & TVS_HASLINES != 0 && (depth > 0 || st & TVS_LINESATROOT != 0) {
            line([bx, mid, bx + indent / 2, mid + 1]);
            line([bx, y, bx + 1, if last_child(id) { mid } else { y + h }]);
            // Ancestors with more below carry on through this row.
            let mut a = node(hwnd, id, |n| n.parent).unwrap_or(0);
            let mut dd = depth - 1;
            while a != 0 {
                if !last_child(a) && (dd > 0 || st & TVS_LINESATROOT != 0) {
                    let ax = indent_x(hwnd, dd) + indent / 2 - sx;
                    line([ax, y, ax + 1, y + h]);
                }
                a = node(hwnd, a, |n| n.parent).unwrap_or(0);
                dd -= 1;
            }
        }
        let open = node(hwnd, id, |n| n.state).unwrap_or(0);
        if st & TVS_HASBUTTONS != 0 && (depth > 0 || st & TVS_LINESATROOT != 0) && shows_children(hwnd, id) {
            let b = [bx - 4, mid - 4, bx + 5, mid + 5];
            unsafe {
                crate::FillRect(hdc, &b, crate::GetSysColorBrush(5));
                let edge = crate::GetSysColorBrush(16);
                crate::FillRect(hdc, &[b[0], b[1], b[2], b[1] + 1], edge);
                crate::FillRect(hdc, &[b[0], b[3] - 1, b[2], b[3]], edge);
                crate::FillRect(hdc, &[b[0], b[1], b[0] + 1, b[3]], edge);
                crate::FillRect(hdc, &[b[2] - 1, b[1], b[2], b[3]], edge);
                let ink = crate::GetSysColorBrush(8);
                crate::FillRect(hdc, &[b[0] + 2, mid, b[2] - 2, mid + 1], ink);
                if open & TVIS_EXPANDED == 0 {
                    crate::FillRect(hdc, &[bx, b[1] + 2, bx + 1, b[3] - 2], ink);
                }
            }
        }
        let mut x = image_x(hwnd, depth) - sx;
        let state_image = ((open & TVIS_STATEIMAGEMASK) >> 12) as i32 - 1;
        if states != 0 && state_image >= 0 {
            crate::imagelist::ImageList_Draw(states, state_image, hdc, x, y, 0);
            x += 16;
        } else if st & TVS_CHECKBOXES != 0 {
            let cb = [x + 1, mid - 6, x + 14, mid + 7];
            unsafe {
                let edge = crate::GetSysColorBrush(16);
                crate::FillRect(hdc, &cb, crate::GetSysColorBrush(5));
                crate::FillRect(hdc, &[cb[0], cb[1], cb[2], cb[1] + 1], edge);
                crate::FillRect(hdc, &[cb[0], cb[3] - 1, cb[2], cb[3]], edge);
                crate::FillRect(hdc, &[cb[0], cb[1], cb[0] + 1, cb[3]], edge);
                crate::FillRect(hdc, &[cb[2] - 1, cb[1], cb[2], cb[3]], edge);
                if state_image == 1 {
                    crate::FillRect(hdc, &[cb[0] + 3, cb[1] + 3, cb[2] - 3, cb[3] - 3], crate::GetSysColorBrush(8));
                }
            }
            x += 16;
        }
        if images != 0 {
            let (normal, sel) = node_images(hwnd, id);
            let i = if open & TVIS_SELECTED != 0 { sel } else { normal };
            let (mut cx, mut cy) = (0, 0);
            unsafe { crate::imagelist::ImageList_GetIconSize(images, &mut cx, &mut cy) };
            if i >= 0 {
                crate::imagelist::ImageList_Draw(images, i, hdc, x, y + (h - cy) / 2, 0);
            }
            x += cx + 2;
        }
        let t = node_text(hwnd, id);
        let tw = crate::text_size(hdc, &t).0;
        let selected = open & TVIS_SELECTED != 0 && (has_focus || st & TVS_SHOWSELALWAYS != 0);
        let hilite = with(hwnd, |tr| tr.drop_hilite == id).unwrap_or(false);
        let label = if st & TVS_FULLROWSELECT != 0 { [0, y, c[2], y + h] } else { [x, y, x + tw + 4, y + h] };
        let mut color = if text_color == -1 { unsafe { crate::GetSysColor(8) } } else { text_color as u32 };
        if selected || hilite {
            let (b, f) = if has_focus || hilite { (13, 14) } else { (15, 18) };
            unsafe {
                crate::FillRect(hdc, &label, crate::GetSysColorBrush(b));
                color = crate::GetSysColor(f);
            }
        }
        unsafe { crate::SetTextColor(hdc, color) };
        let mut tt = t.clone();
        let mut tr = [x + 2, y, x + tw + 4, y + h];
        // DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_NOCLIP
        unsafe { crate::DrawTextW(hdc, tt.as_mut_ptr(), tt.len() as i32, &mut tr, 0x20 | 0x4 | 0x800 | 0x100) };
        if open & TVIS_SELECTED != 0 && has_focus {
            unsafe { crate::DrawFocusRect(hdc, &label) };
        }
        let _ = TVIS_BOLD;
    }
    unsafe { crate::SelectObject(hdc, old) };
}

/// (item, part) at client (x, y).
fn hit(hwnd: Hwnd, x: i32, y: i32) -> (u64, u32) {
    let c = client(hwnd);
    if y < 0 || x < 0 || x >= c[2] || y >= c[3] {
        return (0, TVHT_NOWHERE);
    }
    let h = item_height(hwnd).max(1);
    let vis = visible(hwnd);
    let (top, sx, indent) = with(hwnd, |t| (t.top, t.sx, t.indent)).unwrap_or((0, 0, 19));
    let row = y / h + top;
    let Some(&(id, depth)) = vis.get(row as usize) else { return (0, TVHT_NOWHERE) };
    let x = x + sx;
    let st = style(hwnd);
    let bx = indent_x(hwnd, depth) + indent / 2;
    if st & TVS_HASBUTTONS != 0 && (depth > 0 || st & TVS_LINESATROOT != 0) && x >= bx - 6 && x <= bx + 6 && shows_children(hwnd, id) {
        return (id, TVHT_ONITEMBUTTON);
    }
    let ix = image_x(hwnd, depth);
    if x < ix {
        return (id, TVHT_ONITEMINDENT);
    }
    let lx = label_x(hwnd, depth);
    if x < lx {
        let check = st & TVS_CHECKBOXES != 0 || with(hwnd, |t| t.state_images).unwrap_or(0) != 0;
        if check && x < ix + 16 {
            return (id, TVHT_ONITEMSTATEICON);
        }
        return (id, TVHT_ONITEMICON);
    }
    let font = with(hwnd, |t| t.font).unwrap_or(0);
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
    let w = crate::text_size(dc, &node_text(hwnd, id)).0;
    unsafe {
        crate::SelectObject(dc, old);
        crate::ReleaseDC(hwnd, dc);
    }
    if x < lx + w + 4 {
        (id, TVHT_ONITEMLABEL)
    } else {
        (id, TVHT_ONITEMRIGHT)
    }
}

// -----| get and set |-----

fn get_item(hwnd: Hwnd, lp: i64, unicode: bool) -> bool {
    if lp == 0 {
        return false;
    }
    let p = lp as *mut u8;
    let (mask, id, state_mask, dst, cap) = unsafe {
        (
            core::ptr::read_unaligned(p as *const u32),
            core::ptr::read_unaligned(p.add(8) as *const u64),
            core::ptr::read_unaligned(p.add(20) as *const u32),
            core::ptr::read_unaligned(p.add(24) as *const u64),
            core::ptr::read_unaligned(p.add(32) as *const i32),
        )
    };
    let Some(n) = node(hwnd, id, |n| n.clone()) else { return false };
    unsafe {
        if mask & TVIF_TEXT != 0 {
            let t = node_text(hwnd, id);
            crate::put_text(dst, cap, &t, unicode);
        }
        if mask & TVIF_STATE != 0 {
            core::ptr::write_unaligned(p.add(16) as *mut u32, n.state & state_mask);
        }
        let images = node_images(hwnd, id);
        if mask & TVIF_IMAGE != 0 {
            core::ptr::write_unaligned(p.add(36) as *mut i32, images.0);
        }
        if mask & TVIF_SELECTEDIMAGE != 0 {
            core::ptr::write_unaligned(p.add(40) as *mut i32, images.1);
        }
        if mask & TVIF_CHILDREN != 0 {
            let c = match n.has_children {
                Some(CHILDREN_CALLBACK) => dispinfo(hwnd, id, TVIF_CHILDREN).3,
                Some(c) => c,
                None => n.children.len().min(1) as i32,
            };
            core::ptr::write_unaligned(p.add(44) as *mut i32, c);
        }
        if mask & TVIF_PARAM != 0 {
            core::ptr::write_unaligned(p.add(48) as *mut i64, n.param);
        }
    }
    true
}

fn set_item(hwnd: Hwnd, lp: i64, unicode: bool) -> bool {
    if lp == 0 {
        return false;
    }
    let p = lp as *const u8;
    let (mask, id, state, state_mask, text, image, sel, children, param) = unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const i32);
        let q = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u64);
        (d(0) as u32, q(8), d(16) as u32, d(20) as u32, q(24), d(36), d(40), d(44), q(48) as i64)
    };
    if node(hwnd, id, |_| ()).is_none() {
        return false;
    }
    let new_text = (mask & TVIF_TEXT != 0).then(|| if text == TEXT_CALLBACK { None } else { Some(crate::text_arg(text, unicode)) });
    node(hwnd, id, |n| {
        if let Some(t) = new_text {
            n.text = t;
        }
        if mask & TVIF_STATE != 0 {
            // The selection only changes through TVM_SELECTITEM.
            let m = state_mask & !TVIS_SELECTED;
            n.state = (n.state & !m) | (state & m);
        }
        if mask & TVIF_IMAGE != 0 {
            n.image = image;
        }
        if mask & TVIF_SELECTEDIMAGE != 0 {
            n.sel_image = sel;
        }
        if mask & TVIF_CHILDREN != 0 {
            n.has_children = Some(children);
        }
        if mask & TVIF_PARAM != 0 {
            n.param = param;
        }
    });
    layout(hwnd);
    invalidate(hwnd);
    true
}

fn next_item(hwnd: Hwnd, kind: u32, from: u64) -> u64 {
    let (from_ok, parent, roots, children) = with(hwnd, |t| {
        let n = t.nodes.get(&from);
        (n.is_some(), n.map_or(0, |n| n.parent), t.roots.clone(), n.map_or(Vec::new(), |n| n.children.clone()))
    })
    .unwrap_or_default();
    let sibling = |dir: i32| -> u64 {
        if !from_ok {
            return 0;
        }
        let sibs = if parent == 0 { roots.clone() } else { node(hwnd, parent, |n| n.children.clone()).unwrap_or_default() };
        let Some(at) = sibs.iter().position(|&s| s == from) else { return 0 };
        let k = at as i32 + dir;
        if k < 0 { 0 } else { sibs.get(k as usize).copied().unwrap_or(0) }
    };
    match kind {
        TVGN_ROOT => roots.first().copied().unwrap_or(0),
        TVGN_NEXT => sibling(1),
        TVGN_PREVIOUS => sibling(-1),
        TVGN_PARENT => parent,
        // TVM_GETNEXTITEM with a null item and TVGN_CHILD: the first root.
        TVGN_CHILD => {
            if from == 0 || from == TVI_ROOT {
                roots.first().copied().unwrap_or(0)
            } else {
                children.first().copied().unwrap_or(0)
            }
        }
        TVGN_FIRSTVISIBLE => {
            let top = with(hwnd, |t| t.top).unwrap_or(0);
            visible(hwnd).get(top as usize).map_or(0, |v| v.0)
        }
        TVGN_NEXTVISIBLE | TVGN_PREVIOUSVISIBLE => {
            let vis = visible(hwnd);
            let Some(at) = vis.iter().position(|v| v.0 == from) else { return 0 };
            if kind == TVGN_NEXTVISIBLE {
                vis.get(at + 1).map_or(0, |v| v.0)
            } else if at > 0 {
                vis[at - 1].0
            } else {
                0
            }
        }
        TVGN_DROPHILITE => with(hwnd, |t| t.drop_hilite).unwrap_or(0),
        TVGN_CARET => with(hwnd, |t| t.selected).unwrap_or(0),
        TVGN_LASTVISIBLE => visible(hwnd).last().map_or(0, |v| v.0),
        _ => 0,
    }
}

fn sort_children(hwnd: Hwnd, parent: u64, recurse: bool, cmp: Option<(usize, i64)>) -> bool {
    let parent = if parent == TVI_ROOT { 0 } else { parent };
    let mut kids: Vec<u64> = match with(hwnd, |t| {
        if parent == 0 {
            Some(t.roots.clone())
        } else {
            t.nodes.get(&parent).map(|n| n.children.clone())
        }
    })
    .flatten()
    {
        Some(k) => k,
        None => return false,
    };
    match cmp {
        Some((f, lp)) => {
            let f: unsafe extern "system" fn(i64, i64, i64) -> i32 = unsafe { core::mem::transmute(f) };
            let param = |id: u64| node(hwnd, id, |n| n.param).unwrap_or(0);
            crate::merge_sort(&mut kids, &mut |a, b| unsafe { f(param(*a), param(*b), lp) });
        }
        None => {
            let key = |id: u64| node_text(hwnd, id).iter().map(|&c| crate::lower(c)).collect::<Vec<u16>>();
            crate::merge_sort(&mut kids, &mut |a, b| match key(*a).cmp(&key(*b)) {
                core::cmp::Ordering::Less => -1,
                core::cmp::Ordering::Equal => 0,
                core::cmp::Ordering::Greater => 1,
            });
        }
    }
    with(hwnd, |t| {
        if let Some(s) = siblings_of(t, parent) {
            s.clone_from(&kids);
        }
    });
    if recurse {
        for k in kids {
            sort_children(hwnd, k, true, cmp);
        }
    }
    invalidate(hwnd);
    true
}

// -----| label editing |-----

/// NMTVDISPINFO with `text`.
fn label_notify(hwnd: Hwnd, id: u64, code_w: i32, code_a: i32, text: Option<&[u16]>) -> i64 {
    let unicode = crate::parent_unicode(hwnd);
    let mut w: Vec<u16> = text.map_or(Vec::new(), |t| t.to_vec());
    w.push(0);
    let mut narrow = alloc::vec![0u8; w.len() * 2 + 1];
    let ptr: u64 = match (text.is_some(), unicode) {
        (false, _) => 0,
        (true, true) => w.as_ptr() as u64,
        (true, false) => {
            crate::put_text(narrow.as_mut_ptr() as u64, narrow.len() as i32, &w[..w.len() - 1], false);
            narrow.as_ptr() as u64
        }
    };
    let (state, param) = node(hwnd, id, |n| (n.state, n.param)).unwrap_or((0, 0));
    let item = tvitem(TVIF_TEXT | TVIF_PARAM | TVIF_STATE | 0x10, id, state, ptr, 0, 0, 0, 0, param);
    crate::notify(hwnd, if unicode { code_w } else { code_a }, &item)
}

fn start_edit(hwnd: Hwnd, id: u64) -> Hwnd {
    if node(hwnd, id, |_| ()).is_none() {
        return 0;
    }
    let text = node_text(hwnd, id);
    if label_notify(hwnd, id, TVN_BEGINLABELEDITW, TVN_BEGINLABELEDITA, Some(&text)) != 0 {
        return 0;
    }
    ensure_visible(hwnd, id);
    let vis = visible(hwnd);
    let Some(row) = vis.iter().position(|v| v.0 == id) else { return 0 };
    let depth = vis[row].1;
    let (top, sx, font) = with(hwnd, |t| (t.top, t.sx, t.font)).unwrap_or((0, 0, 0));
    let h = item_height(hwnd);
    let y = (row as i32 - top) * h;
    let x = label_x(hwnd, depth) - sx;
    let c = client(hwnd);
    let e = crate::labeledit::start(hwnd, [x, y, c[2], y + h + 2], &text, font);
    with(hwnd, |t| {
        t.edit = e;
        t.edit_item = id;
    });
    e
}

fn end_edit(hwnd: Hwnd, keep: bool) {
    let Some((e, id)) = with(hwnd, |t| (core::mem::replace(&mut t.edit, 0), t.edit_item)) else { return };
    if e == 0 {
        return;
    }
    let text = crate::labeledit::text(e);
    let r = label_notify(hwnd, id, TVN_ENDLABELEDITW, TVN_ENDLABELEDITA, if keep { Some(&text) } else { None });
    if keep && r != 0 {
        node(hwnd, id, |n| {
            if n.text.is_some() {
                n.text = Some(text);
            }
        });
    }
    unsafe {
        crate::DestroyWindow(e);
        crate::SetFocus(hwnd);
    }
    invalidate(hwnd);
}

// -----| input |-----

fn key_down(hwnd: Hwnd, vk: u32) {
    let mut e = [0u8; 8];
    e[0..2].copy_from_slice(&(vk as u16).to_le_bytes());
    crate::notify(hwnd, TVN_KEYDOWN, &e);
    let sel = with(hwnd, |t| t.selected).unwrap_or(0);
    let vis = visible(hwnd);
    let row = vis.iter().position(|v| v.0 == sel);
    let rows = (client(hwnd)[3] / item_height(hwnd).max(1)).max(1) as usize;
    let go = |to: Option<usize>| {
        if let Some(&(id, _)) = to.and_then(|i| vis.get(i)) {
            select(hwnd, id, TVC_BYKEYBOARD);
        }
    };
    let open = node(hwnd, sel, |n| n.state & TVIS_EXPANDED != 0).unwrap_or(false);
    match vk {
        0x26 => go(row.map_or(Some(0), |r| r.checked_sub(1))),
        0x28 => go(row.map_or(Some(0), |r| Some(r + 1))),
        0x24 => go(Some(0)),
        0x23 => go(vis.len().checked_sub(1)),
        0x21 => go(row.map(|r| r.saturating_sub(rows))),
        0x22 => go(row.map(|r| (r + rows).min(vis.len().saturating_sub(1)))),
        0x25 => {
            if open {
                expand(hwnd, sel, TVE_COLLAPSE, true);
            } else if let Some(p) = node(hwnd, sel, |n| n.parent).filter(|&p| p != 0) {
                select(hwnd, p, TVC_BYKEYBOARD);
            }
        }
        0x27 => {
            if sel != 0 && !open && shows_children(hwnd, sel) {
                expand(hwnd, sel, TVE_EXPAND, true);
            } else if open {
                go(row.map(|r| r + 1));
            }
        }
        0x6B if sel != 0 => {
            expand(hwnd, sel, TVE_EXPAND, true);
        }
        0x6D if sel != 0 => {
            expand(hwnd, sel, TVE_COLLAPSE, true);
        }
        0x71 if style(hwnd) & TVS_EDITLABELS != 0 && sel != 0 => {
            start_edit(hwnd, sel);
        }
        0x20 if style(hwnd) & TVS_CHECKBOXES != 0 && sel != 0 => toggle_check(hwnd, sel),
        0x0D => {
            crate::notify(hwnd, NM_RETURN, &[]);
        }
        _ => {}
    }
}

fn toggle_check(hwnd: Hwnd, id: u64) {
    node(hwnd, id, |n| {
        let s = (n.state & TVIS_STATEIMAGEMASK) >> 12;
        let next = if s == 2 { 1 } else { 2 };
        n.state = (n.state & !TVIS_STATEIMAGEMASK) | (next << 12);
    });
    invalidate(hwnd);
}

unsafe extern "system" fn tree_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let def = || unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        0x0081 => {
            TREES.lock().insert(
                hwnd,
                Tree {
                    nodes: BTreeMap::new(),
                    roots: Vec::new(),
                    next: 0x0002_0000,
                    selected: 0,
                    drop_hilite: 0,
                    top: 0,
                    sx: 0,
                    font: 0,
                    images: 0,
                    state_images: 0,
                    indent: 19,
                    item_h: 0,
                    bk: -1,
                    text_color: -1,
                    line_color: 0xFF00_0000,
                    ex: 0,
                    edit: 0,
                    edit_item: 0,
                    has_focus: false,
                },
            );
            def()
        }
        0x0002 => {
            delete(hwnd, TVI_ROOT);
            0
        }
        0x0082 => {
            TREES.lock().remove(&hwnd);
            def()
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0005 => {
            layout(hwnd);
            0
        }
        0x0030 => {
            with(hwnd, |t| t.font = wp);
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0031 => with(hwnd, |t| t.font as i64).unwrap_or(0),
        0x0007 => {
            with(hwnd, |t| t.has_focus = true);
            crate::notify(hwnd, NM_SETFOCUS, &[]);
            invalidate(hwnd);
            0
        }
        0x0008 => {
            with(hwnd, |t| t.has_focus = false);
            crate::notify(hwnd, NM_KILLFOCUS, &[]);
            invalidate(hwnd);
            0
        }
        0x0087 => 0x1 | 0x4, // DLGC_WANTARROWS | DLGC_WANTCHARS
        0x0100 => {
            key_down(hwnd, wp as u32);
            0
        }
        crate::labeledit::DONE => {
            end_edit(hwnd, wp != 0);
            0
        }
        0x0115 | 0x020A => {
            let total = visible(hwnd).len() as i32;
            let rows = (client(hwnd)[3] / item_height(hwnd).max(1)).max(1);
            let pos = with(hwnd, |t| t.top).unwrap_or(0);
            let target = if msg == 0x020A {
                let delta = ((wp >> 16) & 0xFFFF) as i16 as i32;
                pos - delta * 3 / 120
            } else {
                crate::scroll_target(hwnd, 1, wp, 1, rows, pos, (total - rows).max(0))
            };
            with(hwnd, |t| t.top = target);
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0114 => {
            let c = client(hwnd);
            let pos = with(hwnd, |t| t.sx).unwrap_or(0);
            let mut si = [28u32, 7, 0, 0, 0, 0, 0];
            unsafe { crate::GetScrollInfo(hwnd, 0, si.as_mut_ptr()) };
            let target = crate::scroll_target(hwnd, 0, wp, 20, c[2], pos, (si[3] as i32 + 1 - c[2]).max(0));
            with(hwnd, |t| t.sx = target);
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0201 | 0x0203 | 0x0204 | 0x0206 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            unsafe { crate::SetFocus(hwnd) };
            let (id, part) = hit(hwnd, x, y);
            let double = msg == 0x0203 || msg == 0x0206;
            let right = msg == 0x0204 || msg == 0x0206;
            if !right && !double {
                match part {
                    TVHT_ONITEMBUTTON => {
                        expand(hwnd, id, TVE_TOGGLE, true);
                    }
                    TVHT_ONITEMSTATEICON => toggle_check(hwnd, id),
                    TVHT_ONITEMICON | TVHT_ONITEMLABEL => {
                        let was = with(hwnd, |t| t.selected).unwrap_or(0);
                        select(hwnd, id, TVC_BYMOUSE);
                        if part == TVHT_ONITEMLABEL && was == id && style(hwnd) & TVS_EDITLABELS != 0 {
                            start_edit(hwnd, id);
                        }
                    }
                    _ => {
                        if style(hwnd) & TVS_FULLROWSELECT != 0 && id != 0 {
                            select(hwnd, id, TVC_BYMOUSE);
                        }
                    }
                }
                if style(hwnd) & TVS_SINGLEEXPAND != 0 && id != 0 && part & (TVHT_ONITEMICON | TVHT_ONITEMLABEL) != 0 {
                    expand(hwnd, id, TVE_TOGGLE, true);
                }
            }
            let code = match (right, double) {
                (false, false) => NM_CLICK,
                (false, true) => NM_DBLCLK,
                (true, false) => NM_RCLICK,
                (true, true) => -6,
            };
            let handled = crate::notify(hwnd, code, &[]);
            if double && !right && handled == 0 && id != 0 && part & (TVHT_ONITEMICON | TVHT_ONITEMLABEL) != 0 {
                expand(hwnd, id, TVE_TOGGLE, true);
            }
            0
        }

        TVM_INSERTITEMW | TVM_INSERTITEMA => insert(hwnd, lp, msg == TVM_INSERTITEMW),
        TVM_DELETEITEM => delete(hwnd, lp as u64) as i64,
        TVM_EXPAND => expand(hwnd, lp as u64, wp as u32, false) as i64,
        TVM_GETITEMRECT => {
            if lp == 0 {
                return 0;
            }
            let id = unsafe { *(lp as *const u64) };
            let vis = visible(hwnd);
            let Some(row) = vis.iter().position(|v| v.0 == id) else { return 0 };
            let (top, sx, font) = with(hwnd, |t| (t.top, t.sx, t.font)).unwrap_or((0, 0, 0));
            let h = item_height(hwnd);
            let y = (row as i32 - top) * h;
            let c = client(hwnd);
            let out = if wp != 0 {
                let x = label_x(hwnd, vis[row].1) - sx;
                let dc = unsafe { crate::GetDC(hwnd) };
                let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
                let w = crate::text_size(dc, &node_text(hwnd, id)).0;
                unsafe {
                    crate::SelectObject(dc, old);
                    crate::ReleaseDC(hwnd, dc);
                }
                [x, y, x + w + 4, y + h]
            } else {
                [0, y, c[2], y + h]
            };
            unsafe { *(lp as *mut [i32; 4]) = out };
            1
        }
        TVM_GETCOUNT => with(hwnd, |t| t.nodes.len() as i64).unwrap_or(0),
        TVM_GETINDENT => with(hwnd, |t| t.indent as i64).unwrap_or(0),
        TVM_SETINDENT => {
            with(hwnd, |t| t.indent = (wp as i32).max(5));
            invalidate(hwnd);
            0
        }
        TVM_GETIMAGELIST => with(hwnd, |t| if wp == 2 { t.state_images as i64 } else { t.images as i64 }).unwrap_or(0),
        TVM_SETIMAGELIST => {
            let old = with(hwnd, |t| {
                if wp == 2 {
                    core::mem::replace(&mut t.state_images, lp as u64)
                } else {
                    core::mem::replace(&mut t.images, lp as u64)
                }
            })
            .unwrap_or(0);
            layout(hwnd);
            invalidate(hwnd);
            old as i64
        }
        TVM_GETNEXTITEM => next_item(hwnd, wp as u32, lp as u64) as i64,
        TVM_SELECTITEM => match wp as u32 & 0xFF {
            TVGN_CARET => select(hwnd, lp as u64, TVC_UNKNOWN) as i64,
            TVGN_DROPHILITE => {
                with(hwnd, |t| t.drop_hilite = lp as u64);
                invalidate(hwnd);
                1
            }
            TVGN_FIRSTVISIBLE => {
                let vis = visible(hwnd);
                match vis.iter().position(|v| v.0 == lp as u64) {
                    Some(r) => {
                        with(hwnd, |t| t.top = r as i32);
                        layout(hwnd);
                        invalidate(hwnd);
                        1
                    }
                    None => 0,
                }
            }
            _ => 0,
        },
        TVM_GETITEMW | TVM_GETITEMA => get_item(hwnd, lp, msg == TVM_GETITEMW) as i64,
        TVM_SETITEMW | TVM_SETITEMA => set_item(hwnd, lp, msg == TVM_SETITEMW) as i64,
        TVM_EDITLABELW | TVM_EDITLABELA => start_edit(hwnd, lp as u64) as i64,
        TVM_GETEDITCONTROL => with(hwnd, |t| t.edit as i64).unwrap_or(0),
        TVM_ENDEDITLABELNOW => {
            end_edit(hwnd, wp == 0);
            1
        }
        TVM_GETVISIBLECOUNT => (client(hwnd)[3] / item_height(hwnd).max(1)) as i64,
        TVM_HITTEST => {
            if lp == 0 {
                return 0;
            }
            let p = lp as *mut u8;
            let (x, y) = unsafe { (*(p as *const i32), *(p as *const i32).add(1)) };
            let (id, part) = hit(hwnd, x, y);
            unsafe {
                core::ptr::write_unaligned(p.add(8) as *mut u32, part);
                core::ptr::write_unaligned(p.add(16) as *mut u64, id);
            }
            id as i64
        }
        TVM_SORTCHILDREN => sort_children(hwnd, lp as u64, wp != 0, None) as i64,
        TVM_SORTCHILDRENCB => {
            if lp == 0 {
                return 0;
            }
            // TVSORTCB: parent, callback, lParam.
            let p = lp as *const u64;
            let (parent, cb, data) = unsafe { (*p, *p.add(1), *p.add(2)) };
            sort_children(hwnd, parent, wp != 0, Some((cb as usize, data as i64))) as i64
        }
        TVM_ENSUREVISIBLE => {
            let before = visible(hwnd).iter().any(|v| v.0 == lp as u64);
            ensure_visible(hwnd, lp as u64);
            before as i64
        }
        TVM_SETITEMHEIGHT => {
            let old = item_height(hwnd);
            with(hwnd, |t| t.item_h = wp as i16 as i32);
            layout(hwnd);
            invalidate(hwnd);
            old as i64
        }
        TVM_GETITEMHEIGHT => item_height(hwnd) as i64,
        TVM_SETBKCOLOR => {
            let old = with(hwnd, |t| core::mem::replace(&mut t.bk, lp as i32)).unwrap_or(-1);
            invalidate(hwnd);
            old as i64
        }
        TVM_GETBKCOLOR => with(hwnd, |t| t.bk as i64).unwrap_or(-1),
        TVM_SETTEXTCOLOR => {
            let old = with(hwnd, |t| core::mem::replace(&mut t.text_color, lp as i32)).unwrap_or(-1);
            invalidate(hwnd);
            old as i64
        }
        TVM_GETTEXTCOLOR => with(hwnd, |t| t.text_color as i64).unwrap_or(-1),
        // Asking about expansion also says if it ever was.
        TVM_GETITEMSTATE => {
            let mask = lp as u32 | if lp as u32 & TVIS_EXPANDED != 0 { TVIS_EXPANDEDONCE } else { 0 };
            node(hwnd, wp, |n| (n.state & mask) as i64).unwrap_or(0)
        }
        TVM_SETLINECOLOR => {
            let old = with(hwnd, |t| core::mem::replace(&mut t.line_color, lp as u32)).unwrap_or(0xFF00_0000);
            invalidate(hwnd);
            old as i64
        }
        TVM_GETLINECOLOR => with(hwnd, |t| t.line_color as i64).unwrap_or(0xFF00_0000),
        TVM_SETEXTENDEDSTYLE => {
            let mask = if wp == 0 { u32::MAX } else { wp as u32 };
            with(hwnd, |t| t.ex = (t.ex & !mask) | (lp as u32 & mask));
            0
        }
        TVM_GETEXTENDEDSTYLE => with(hwnd, |t| t.ex as i64).unwrap_or(0),
        _ => def(),
    }
}
