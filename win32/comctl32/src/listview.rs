//! The list view ("SysListView32"): items with subitems in icon, small icon,
//! list and report views. Report view has a header control; items may keep
//! their text and images themselves or ask the parent (LVN_GETDISPINFO), and
//! LVS_OWNERDATA lists keep only a count and the selection. Selection,
//! keyboard, sorting, finding, label editing and the notifications go as
//! Windows has them. Not yet: groups, tiles, custom draw, marquee selection
//! and dragging (the desktop reports clicks, not movement).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const LVM_FIRST: u32 = 0x1000;
const LVM_GETBKCOLOR: u32 = LVM_FIRST;
const LVM_SETBKCOLOR: u32 = LVM_FIRST + 1;
const LVM_GETIMAGELIST: u32 = LVM_FIRST + 2;
const LVM_SETIMAGELIST: u32 = LVM_FIRST + 3;
const LVM_GETITEMCOUNT: u32 = LVM_FIRST + 4;
const LVM_GETITEMA: u32 = LVM_FIRST + 5;
const LVM_SETITEMA: u32 = LVM_FIRST + 6;
const LVM_INSERTITEMA: u32 = LVM_FIRST + 7;
const LVM_DELETEITEM: u32 = LVM_FIRST + 8;
const LVM_DELETEALLITEMS: u32 = LVM_FIRST + 9;
const LVM_GETCALLBACKMASK: u32 = LVM_FIRST + 10;
const LVM_SETCALLBACKMASK: u32 = LVM_FIRST + 11;
const LVM_GETNEXTITEM: u32 = LVM_FIRST + 12;
const LVM_FINDITEMA: u32 = LVM_FIRST + 13;
const LVM_GETITEMRECT: u32 = LVM_FIRST + 14;
const LVM_SETITEMPOSITION: u32 = LVM_FIRST + 15;
const LVM_GETITEMPOSITION: u32 = LVM_FIRST + 16;
const LVM_GETSTRINGWIDTHA: u32 = LVM_FIRST + 17;
const LVM_HITTEST: u32 = LVM_FIRST + 18;
const LVM_ENSUREVISIBLE: u32 = LVM_FIRST + 19;
const LVM_SCROLL: u32 = LVM_FIRST + 20;
const LVM_REDRAWITEMS: u32 = LVM_FIRST + 21;
const LVM_ARRANGE: u32 = LVM_FIRST + 22;
const LVM_EDITLABELA: u32 = LVM_FIRST + 23;
const LVM_GETEDITCONTROL: u32 = LVM_FIRST + 24;
const LVM_GETCOLUMNA: u32 = LVM_FIRST + 25;
const LVM_SETCOLUMNA: u32 = LVM_FIRST + 26;
const LVM_INSERTCOLUMNA: u32 = LVM_FIRST + 27;
const LVM_DELETECOLUMN: u32 = LVM_FIRST + 28;
const LVM_GETCOLUMNWIDTH: u32 = LVM_FIRST + 29;
const LVM_SETCOLUMNWIDTH: u32 = LVM_FIRST + 30;
const LVM_GETHEADER: u32 = LVM_FIRST + 31;
const LVM_GETVIEWRECT: u32 = LVM_FIRST + 34;
const LVM_GETTEXTCOLOR: u32 = LVM_FIRST + 35;
const LVM_SETTEXTCOLOR: u32 = LVM_FIRST + 36;
const LVM_GETTEXTBKCOLOR: u32 = LVM_FIRST + 37;
const LVM_SETTEXTBKCOLOR: u32 = LVM_FIRST + 38;
const LVM_GETTOPINDEX: u32 = LVM_FIRST + 39;
const LVM_GETCOUNTPERPAGE: u32 = LVM_FIRST + 40;
const LVM_GETORIGIN: u32 = LVM_FIRST + 41;
const LVM_UPDATE: u32 = LVM_FIRST + 42;
const LVM_SETITEMSTATE: u32 = LVM_FIRST + 43;
const LVM_GETITEMSTATE: u32 = LVM_FIRST + 44;
const LVM_GETITEMTEXTA: u32 = LVM_FIRST + 45;
const LVM_SETITEMTEXTA: u32 = LVM_FIRST + 46;
const LVM_SETITEMCOUNT: u32 = LVM_FIRST + 47;
const LVM_SORTITEMS: u32 = LVM_FIRST + 48;
const LVM_SETITEMPOSITION32: u32 = LVM_FIRST + 49;
const LVM_GETSELECTEDCOUNT: u32 = LVM_FIRST + 50;
const LVM_GETITEMSPACING: u32 = LVM_FIRST + 51;
const LVM_SETICONSPACING: u32 = LVM_FIRST + 53;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = LVM_FIRST + 54;
const LVM_GETEXTENDEDLISTVIEWSTYLE: u32 = LVM_FIRST + 55;
const LVM_GETSUBITEMRECT: u32 = LVM_FIRST + 56;
const LVM_SUBITEMHITTEST: u32 = LVM_FIRST + 57;
const LVM_SETCOLUMNORDERARRAY: u32 = LVM_FIRST + 58;
const LVM_GETCOLUMNORDERARRAY: u32 = LVM_FIRST + 59;
const LVM_GETSELECTIONMARK: u32 = LVM_FIRST + 66;
const LVM_SETSELECTIONMARK: u32 = LVM_FIRST + 67;
const LVM_GETITEMW: u32 = LVM_FIRST + 75;
const LVM_SETITEMW: u32 = LVM_FIRST + 76;
const LVM_INSERTITEMW: u32 = LVM_FIRST + 77;
const LVM_SORTITEMSEX: u32 = LVM_FIRST + 81;
const LVM_FINDITEMW: u32 = LVM_FIRST + 83;
const LVM_GETSTRINGWIDTHW: u32 = LVM_FIRST + 87;
const LVM_GETCOLUMNW: u32 = LVM_FIRST + 95;
const LVM_SETCOLUMNW: u32 = LVM_FIRST + 96;
const LVM_INSERTCOLUMNW: u32 = LVM_FIRST + 97;
const LVM_GETITEMTEXTW: u32 = LVM_FIRST + 115;
const LVM_SETITEMTEXTW: u32 = LVM_FIRST + 116;
const LVM_EDITLABELW: u32 = LVM_FIRST + 118;
const LVM_SETVIEW: u32 = LVM_FIRST + 142;
const LVM_GETVIEW: u32 = LVM_FIRST + 143;
const LVM_ISITEMVISIBLE: u32 = LVM_FIRST + 182;

const LVS_REPORT: u32 = 1;
const LVS_SMALLICON: u32 = 2;
const LVS_LIST: u32 = 3;
const LVS_SINGLESEL: u32 = 4;
const LVS_SHOWSELALWAYS: u32 = 8;
const LVS_SORTASCENDING: u32 = 0x10;
const LVS_SORTDESCENDING: u32 = 0x20;
const LVS_EDITLABELS: u32 = 0x200;
const LVS_OWNERDATA: u32 = 0x1000;
const LVS_NOCOLUMNHEADER: u32 = 0x4000;
const LVS_NOSORTHEADER: u32 = 0x8000;

const LVS_EX_GRIDLINES: u32 = 0x1;
const LVS_EX_CHECKBOXES: u32 = 0x4;
const LVS_EX_FULLROWSELECT: u32 = 0x20;

const LVIF_TEXT: u32 = 0x1;
const LVIF_IMAGE: u32 = 0x2;
const LVIF_PARAM: u32 = 0x4;
const LVIF_STATE: u32 = 0x8;
const LVIF_INDENT: u32 = 0x10;
const LVIF_NORECOMPUTE: u32 = 0x800;

const LVIS_FOCUSED: u32 = 1;
const LVIS_SELECTED: u32 = 2;
const LVIS_STATEIMAGEMASK: u32 = 0xF000;

const LVCF_FMT: u32 = 1;
const LVCF_WIDTH: u32 = 2;
const LVCF_TEXT: u32 = 4;
const LVCF_SUBITEM: u32 = 8;
const LVCF_IMAGE: u32 = 0x10;
const LVCF_ORDER: u32 = 0x20;

const LVNI_FOCUSED: u32 = 1;
const LVNI_SELECTED: u32 = 2;
const LVNI_CUT: u32 = 4;
const LVNI_DROPHILITED: u32 = 8;
const LVNI_ABOVE: u32 = 0x100;
const LVNI_BELOW: u32 = 0x200;
const LVNI_TOLEFT: u32 = 0x400;
const LVNI_TORIGHT: u32 = 0x800;

const LVFI_PARAM: u32 = 1;
const LVFI_STRING: u32 = 2;
const LVFI_PARTIAL: u32 = 8;
const LVFI_WRAP: u32 = 0x20;

const LVHT_NOWHERE: u32 = 1;
const LVHT_ONITEMICON: u32 = 2;
const LVHT_ONITEMLABEL: u32 = 4;
const LVHT_ONITEMSTATEICON: u32 = 8;

const LVIR_BOUNDS: i32 = 0;
const LVIR_ICON: i32 = 1;
const LVIR_LABEL: i32 = 2;

const LVN_ITEMCHANGING: i32 = -100;
const LVN_ITEMCHANGED: i32 = -101;
const LVN_INSERTITEM: i32 = -102;
const LVN_DELETEITEM: i32 = -103;
const LVN_DELETEALLITEMS: i32 = -104;
const LVN_BEGINLABELEDITA: i32 = -105;
const LVN_ENDLABELEDITA: i32 = -106;
const LVN_COLUMNCLICK: i32 = -108;
const LVN_ITEMACTIVATE: i32 = -114;
const LVN_GETDISPINFOA: i32 = -150;
const LVN_KEYDOWN: i32 = -155;
const LVN_BEGINLABELEDITW: i32 = -175;
const LVN_ENDLABELEDITW: i32 = -176;
const LVN_GETDISPINFOW: i32 = -177;

const NM_CLICK: i32 = -2;
const NM_DBLCLK: i32 = -3;
const NM_RETURN: i32 = -4;
const NM_RCLICK: i32 = -5;
const NM_SETFOCUS: i32 = -7;
const NM_KILLFOCUS: i32 = -8;

const CLR_NONE: u32 = 0xFFFF_FFFF;
const CLR_DEFAULT: u32 = 0xFF00_0000;
const TEXT_CALLBACK: u64 = u64::MAX;
const IMAGE_CALLBACK: i32 = -1;

const HDM_INSERTITEMW: u32 = 0x120A;
const HDM_DELETEITEM: u32 = 0x1202;
const HDM_SETITEMW: u32 = 0x120C;
const HDM_LAYOUT: u32 = 0x1205;
const HDN_ITEMCLICKW: i32 = -322;
const HDN_ITEMCHANGEDW: i32 = -321;

const WM_HSCROLL: u32 = 0x0114;
const WM_VSCROLL: u32 = 0x0115;

/// A cell's text: its own, or asked of the parent.
#[derive(Clone)]
enum Text {
    Own(Vec<u16>),
    Callback,
}

#[derive(Clone)]
struct Item {
    text: Text,
    subs: Vec<Option<Text>>,
    image: i32,
    param: i64,
    state: u32,
    indent: i32,
    /// Where it was put (LVM_SETITEMPOSITION), in the icon views.
    pos: Option<(i32, i32)>,
}

#[derive(Clone)]
struct Column {
    text: Vec<u16>,
    width: i32,
    fmt: i32,
    /// The subitem it shows, and what LVM_GETCOLUMN reports (0 unless given).
    sub: i32,
    given_sub: i32,
    image: i32,
    order: i32,
}

struct List {
    items: Vec<Item>,
    columns: Vec<Column>,
    header: Hwnd,
    font: u64,
    ex: u32,
    /// Image lists: normal, small, state.
    images: [u64; 3],
    bk: u32,
    text_color: u32,
    text_bk: u32,
    /// Scrolled, in pixels.
    sx: i32,
    sy: i32,
    focus: i32,
    mark: i32,
    callback_mask: u32,
    /// LVS_OWNERDATA: the count and the selection.
    virtual_count: i32,
    virtual_sel: BTreeSet<i32>,
    edit: Hwnd,
    edit_item: i32,
    has_focus: bool,
    icon_spacing: (i32, i32),
}

static LISTS: Lock<BTreeMap<Hwnd, List>> = Lock::new(BTreeMap::new());

fn with<R>(hwnd: Hwnd, f: impl FnOnce(&mut List) -> R) -> Option<R> {
    LISTS.lock().get_mut(&hwnd).map(f)
}

pub fn register_class() {
    crate::register("SysListView32", list_proc as *const () as usize, true);
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

fn view(hwnd: Hwnd) -> u32 {
    style(hwnd) & 3
}

fn owner_data(hwnd: Hwnd) -> bool {
    style(hwnd) & LVS_OWNERDATA != 0
}

fn count(hwnd: Hwnd) -> i32 {
    let od = owner_data(hwnd);
    with(hwnd, |l| if od { l.virtual_count } else { l.items.len() as i32 }).unwrap_or(0)
}

fn client(hwnd: Hwnd) -> [i32; 4] {
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut r) };
    r
}

fn invalidate(hwnd: Hwnd) {
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

// -----| geometry |-----

fn small_size(hwnd: Hwnd) -> (i32, i32) {
    let il = with(hwnd, |l| l.images[1]).unwrap_or(0);
    let (mut cx, mut cy) = (0, 0);
    if il != 0 {
        unsafe { crate::imagelist::ImageList_GetIconSize(il, &mut cx, &mut cy) };
    }
    (cx, cy)
}

fn normal_size(hwnd: Hwnd) -> (i32, i32) {
    let il = with(hwnd, |l| l.images[0]).unwrap_or(0);
    let (mut cx, mut cy) = (32, 32);
    if il != 0 {
        unsafe { crate::imagelist::ImageList_GetIconSize(il, &mut cx, &mut cy) };
    }
    (cx, cy)
}

fn line_height(hwnd: Hwnd) -> i32 {
    crate::font_height(hwnd, with(hwnd, |l| l.font).unwrap_or(0))
}

/// A row's height in the small-icon, list and report views.
fn row_height(hwnd: Hwnd) -> i32 {
    (line_height(hwnd) + 2).max(small_size(hwnd).1 + 1).max(1)
}

fn header_height(hwnd: Hwnd) -> i32 {
    if view(hwnd) != LVS_REPORT || style(hwnd) & LVS_NOCOLUMNHEADER != 0 {
        return 0;
    }
    let h = with(hwnd, |l| l.header).unwrap_or(0);
    if h == 0 {
        return 0;
    }
    let mut r = [0i32; 4];
    unsafe { crate::GetClientRect(h, &mut r) };
    r[3]
}

/// The width of a cell in the list and small-icon views.
fn cell_width(hwnd: Hwnd) -> i32 {
    let col = with(hwnd, |l| l.columns.first().map_or(0, |c| c.width)).unwrap_or(0);
    if view(hwnd) == LVS_LIST && col > 0 {
        return col;
    }
    let n = count(hwnd).min(256);
    let dc = unsafe { crate::GetDC(hwnd) };
    let old = unsafe { crate::SelectObject(dc, crate::font_or_default(with(hwnd, |l| l.font).unwrap_or(0))) };
    let mut w = 0;
    for i in 0..n {
        let t = item_text(hwnd, i, 0);
        w = w.max(crate::text_size(dc, &t).0);
    }
    unsafe {
        crate::SelectObject(dc, old);
        crate::ReleaseDC(hwnd, dc);
    }
    w + small_size(hwnd).0 + 10
}

/// An item's bounds in view coordinates (before scrolling).
fn view_rect(hwnd: Hwnd, i: i32) -> [i32; 4] {
    let c = client(hwnd);
    let (cw, ch) = (c[2], c[3]);
    match view(hwnd) {
        LVS_REPORT => {
            let h = row_height(hwnd);
            let w = columns_width(hwnd);
            [0, i * h, w, (i + 1) * h]
        }
        LVS_LIST => {
            let h = row_height(hwnd);
            let per = (ch / h).max(1);
            let w = cell_width(hwnd);
            let (col, row) = (i / per, i % per);
            [col * w, row * h, (col + 1) * w, (row + 1) * h]
        }
        LVS_SMALLICON => {
            let h = row_height(hwnd);
            let w = cell_width(hwnd).max(1);
            let per = (cw / w).max(1);
            let (col, row) = (i % per, i / per);
            placed(hwnd, i, [col * w, row * h, (col + 1) * w, (row + 1) * h])
        }
        _ => {
            let (sw, sh) = with(hwnd, |l| l.icon_spacing).unwrap_or((75, 75));
            let per = (cw / sw.max(1)).max(1);
            let (col, row) = (i % per, i / per);
            placed(hwnd, i, [col * sw, row * sh, (col + 1) * sw, (row + 1) * sh])
        }
    }
}

fn placed(hwnd: Hwnd, i: i32, r: [i32; 4]) -> [i32; 4] {
    match with(hwnd, |l| l.items.get(i as usize).and_then(|it| it.pos)).flatten() {
        Some((x, y)) => [x, y, x + r[2] - r[0], y + r[3] - r[1]],
        None => r,
    }
}

fn columns_width(hwnd: Hwnd) -> i32 {
    with(hwnd, |l| l.columns.iter().map(|c| c.width).sum()).unwrap_or(0)
}

/// An item's bounds in client coordinates.
fn item_rect(hwnd: Hwnd, i: i32) -> [i32; 4] {
    let r = view_rect(hwnd, i);
    let (sx, sy) = with(hwnd, |l| (l.sx, l.sy)).unwrap_or((0, 0));
    let top = header_height(hwnd);
    [r[0] - sx, r[1] - sy + top, r[2] - sx, r[3] - sy + top]
}

/// Columns in display order: (column index, its x relative to the row).
fn column_layout(hwnd: Hwnd) -> Vec<(usize, i32, i32)> {
    let mut cols: Vec<(usize, i32, i32)> = with(hwnd, |l| l.columns.iter().enumerate().map(|(i, c)| (i, c.order, c.width)).collect()).unwrap_or_default();
    cols.sort_by_key(|c| c.1);
    let mut x = 0;
    cols.into_iter()
        .map(|(i, _, w)| {
            let at = x;
            x += w;
            (i, at, w)
        })
        .collect()
}

/// The icon part and the label part of an item in client coordinates.
fn parts(hwnd: Hwnd, i: i32) -> ([i32; 4], [i32; 4]) {
    let r = item_rect(hwnd, i);
    match view(hwnd) {
        LVS_REPORT | LVS_LIST | LVS_SMALLICON => {
            let (iw, _) = small_size(hwnd);
            let indent = with(hwnd, |l| l.items.get(i as usize).map_or(0, |it| it.indent)).unwrap_or(0) * iw;
            let first = if view(hwnd) == LVS_REPORT {
                let lay = column_layout(hwnd);
                match lay.iter().find(|c| c.0 == 0) {
                    Some(c) => [r[0] + c.1, r[0] + c.1 + c.2],
                    None => [r[0], r[2]],
                }
            } else {
                [r[0], r[2]]
            };
            let label_x = first[0] + indent + iw;
            ([first[0] + indent, r[1], label_x, r[3]], [label_x, r[1], first[1], r[3]])
        }
        _ => {
            let (cx, cy) = normal_size(hwnd);
            let mid = (r[0] + r[2]) / 2;
            let icon = [mid - cx / 2, r[1] + 2, mid + cx / 2, r[1] + 2 + cy];
            (icon, [r[0], icon[3] + 2, r[2], r[3]])
        }
    }
}

/// Scroll bars and the header for the current size and contents.
fn layout(hwnd: Hwnd) {
    let c = client(hwnd);
    let n = count(hwnd);
    let v = view(hwnd);
    let top = header_height(hwnd);
    let h = with(hwnd, |l| l.header).unwrap_or(0);
    if h != 0 {
        let show = v == LVS_REPORT && style(hwnd) & LVS_NOCOLUMNHEADER == 0;
        unsafe { crate::ShowWindow(h, if show { 8 } else { 0 }) };
    }
    let (content_w, content_h) = match v {
        LVS_REPORT => (columns_width(hwnd), n * row_height(hwnd)),
        LVS_LIST => {
            let per = (c[3] / row_height(hwnd)).max(1);
            ((n + per - 1) / per * cell_width(hwnd), 0)
        }
        _ => {
            if n == 0 {
                (0, 0)
            } else {
                let last = view_rect(hwnd, n - 1);
                (0, last[3])
            }
        }
    };
    let page_h = c[3] - top;
    let (sx, sy) = with(hwnd, |l| {
        l.sx = l.sx.clamp(0, (content_w - c[2]).max(0));
        l.sy = l.sy.clamp(0, (content_h - page_h).max(0));
        (l.sx, l.sy)
    })
    .unwrap_or((0, 0));
    crate::set_scroll(hwnd, 1, content_h, page_h, sy);
    crate::set_scroll(hwnd, 0, content_w, c[2], sx);
    place_header(hwnd);
}

/// The header across the top, moved with the horizontal scroll.
fn place_header(hwnd: Hwnd) {
    let h = with(hwnd, |l| l.header).unwrap_or(0);
    if h == 0 || view(hwnd) != LVS_REPORT || style(hwnd) & LVS_NOCOLUMNHEADER != 0 {
        return;
    }
    let c = client(hwnd);
    let sx = with(hwnd, |l| l.sx).unwrap_or(0);
    let mut rc = [0, 0, c[2].max(columns_width(hwnd)) + sx, c[3]];
    let mut pos = [0u8; 40];
    let hl: [u64; 2] = [rc.as_mut_ptr() as u64, pos.as_mut_ptr() as u64];
    unsafe { crate::SendMessageW(h, HDM_LAYOUT, 0, hl.as_ptr() as i64) };
    let height = i32::from_le_bytes(pos[28..32].try_into().unwrap());
    unsafe { crate::SetWindowPos(h, 0, -sx, 0, c[2].max(columns_width(hwnd)) + sx, height, 0x14) };
}

// -----| text and notifications |-----

/// NMHDR + LVITEM: ask the parent for what the item leaves to it.
fn dispinfo(hwnd: Hwnd, i: i32, sub: i32, mask: u32, param: i64) -> (Vec<u16>, i32, u32) {
    let unicode = crate::parent_unicode(hwnd);
    let mut buf = alloc::vec![0u16; 260];
    let mut lvi = [0u8; 88];
    lvi[0..4].copy_from_slice(&mask.to_le_bytes());
    lvi[4..8].copy_from_slice(&i.to_le_bytes());
    lvi[8..12].copy_from_slice(&sub.to_le_bytes());
    lvi[24..32].copy_from_slice(&(buf.as_mut_ptr() as u64).to_le_bytes());
    lvi[32..36].copy_from_slice(&(if unicode { 260i32 } else { 520 }).to_le_bytes());
    lvi[40..48].copy_from_slice(&param.to_le_bytes());
    let code = if unicode { LVN_GETDISPINFOW } else { LVN_GETDISPINFOA };
    // The answer comes back in a copy notify() makes, so build the whole
    // structure here and send it directly.
    let mut msg = [0u8; 24 + 88];
    msg[0..8].copy_from_slice(&hwnd.to_le_bytes());
    let id = unsafe { crate::GetDlgCtrlID(hwnd) } as u64;
    msg[8..16].copy_from_slice(&id.to_le_bytes());
    msg[16..20].copy_from_slice(&(code as u32).to_le_bytes());
    msg[24..112].copy_from_slice(&lvi);
    let parent = unsafe { crate::GetParent(hwnd) };
    unsafe { crate::SendMessageW(parent, crate::WM_NOTIFY, id, msg.as_mut_ptr() as i64) };
    let text_ptr = u64::from_le_bytes(msg[48..56].try_into().unwrap());
    let image = i32::from_le_bytes(msg[60..64].try_into().unwrap());
    let state = u32::from_le_bytes(msg[36..40].try_into().unwrap());
    let text = if mask & LVIF_TEXT != 0 && text_ptr != 0 && text_ptr != TEXT_CALLBACK { crate::text_arg(text_ptr, unicode) } else { Vec::new() };
    (text, image, state)
}

/// The text of cell (`i`, `sub`).
fn item_text(hwnd: Hwnd, i: i32, sub: i32) -> Vec<u16> {
    if owner_data(hwnd) {
        return dispinfo(hwnd, i, sub, LVIF_TEXT, 0).0;
    }
    let cell = with(hwnd, |l| {
        let it = l.items.get(i as usize)?;
        Some(if sub == 0 {
            (it.text.clone(), it.param)
        } else {
            (it.subs.get(sub as usize - 1).cloned().flatten().unwrap_or(Text::Own(Vec::new())), it.param)
        })
    })
    .flatten();
    match cell {
        Some((Text::Own(t), _)) => t,
        Some((Text::Callback, param)) => dispinfo(hwnd, i, sub, LVIF_TEXT, param).0,
        None => Vec::new(),
    }
}

fn item_image(hwnd: Hwnd, i: i32) -> i32 {
    if owner_data(hwnd) {
        return dispinfo(hwnd, i, 0, LVIF_IMAGE, 0).1;
    }
    let got = with(hwnd, |l| l.items.get(i as usize).map(|it| (it.image, it.param))).flatten();
    match got {
        Some((IMAGE_CALLBACK, param)) => dispinfo(hwnd, i, 0, LVIF_IMAGE, param).1,
        Some((image, _)) => image,
        None => -1,
    }
}

fn get_state(hwnd: Hwnd, i: i32) -> u32 {
    let od = owner_data(hwnd);
    with(hwnd, |l| {
        if od {
            let mut s = 0;
            if l.virtual_sel.contains(&i) {
                s |= LVIS_SELECTED;
            }
            if l.focus == i {
                s |= LVIS_FOCUSED;
            }
            s
        } else {
            l.items.get(i as usize).map_or(0, |it| it.state)
        }
    })
    .unwrap_or(0)
}

/// NMLISTVIEW after the NMHDR.
fn nmlistview(hwnd: Hwnd, code: i32, item: i32, sub: i32, new: u32, old: u32, changed: u32, pt: (i32, i32), param: i64) -> i64 {
    let mut e = [0u8; 40];
    e[0..4].copy_from_slice(&item.to_le_bytes());
    e[4..8].copy_from_slice(&sub.to_le_bytes());
    e[8..12].copy_from_slice(&new.to_le_bytes());
    e[12..16].copy_from_slice(&old.to_le_bytes());
    e[16..20].copy_from_slice(&changed.to_le_bytes());
    e[20..24].copy_from_slice(&pt.0.to_le_bytes());
    e[24..28].copy_from_slice(&pt.1.to_le_bytes());
    e[32..40].copy_from_slice(&param.to_le_bytes());
    crate::notify(hwnd, code, &e)
}

fn item_param(hwnd: Hwnd, i: i32) -> i64 {
    with(hwnd, |l| l.items.get(i as usize).map_or(0, |it| it.param)).unwrap_or(0)
}

/// Change one item's state, with LVN_ITEMCHANGING / LVN_ITEMCHANGED.
fn set_one(hwnd: Hwnd, i: i32, state: u32, mask: u32) -> bool {
    let old = get_state(hwnd, i);
    let new = (old & !mask) | (state & mask);
    if new == old {
        return true;
    }
    let param = item_param(hwnd, i);
    let (shown_new, shown_old) = (new & mask, old & mask);
    if !owner_data(hwnd) && nmlistview(hwnd, LVN_ITEMCHANGING, i, 0, shown_new, shown_old, LVIF_STATE, (0, 0), param) != 0 {
        return false;
    }
    let od = owner_data(hwnd);
    with(hwnd, |l| {
        if od {
            if new & LVIS_SELECTED != 0 {
                l.virtual_sel.insert(i);
            } else {
                l.virtual_sel.remove(&i);
            }
        } else if let Some(it) = l.items.get_mut(i as usize) {
            it.state = new;
        }
        if new & LVIS_FOCUSED != 0 {
            l.focus = i;
        } else if l.focus == i && mask & LVIS_FOCUSED != 0 {
            l.focus = -1;
        }
    });
    invalidate(hwnd);
    nmlistview(hwnd, LVN_ITEMCHANGED, i, 0, shown_new, shown_old, LVIF_STATE, (0, 0), param);
    true
}

/// LVM_SETITEMSTATE's rules: -1 is every item; one selection with
/// LVS_SINGLESEL; one focus.
fn set_state(hwnd: Hwnd, i: i32, state: u32, mask: u32) -> bool {
    if i == -1 {
        // A virtual list says so once, for all.
        if owner_data(hwnd) {
            let (old_sel, n) = with(hwnd, |l| (l.virtual_sel.clone(), l.virtual_count)).unwrap_or_default();
            if mask & LVIS_SELECTED != 0 {
                with(hwnd, |l| {
                    l.virtual_sel.clear();
                    if state & LVIS_SELECTED != 0 {
                        l.virtual_sel.extend(0..n);
                    }
                });
                let (new, old) = if state & LVIS_SELECTED != 0 { (LVIS_SELECTED, 0) } else { (0, LVIS_SELECTED) };
                if !old_sel.is_empty() || new != 0 {
                    nmlistview(hwnd, LVN_ITEMCHANGED, -1, 0, new, old, LVIF_STATE, (0, 0), 0);
                }
                invalidate(hwnd);
            }
            return true;
        }
        let mut ok = true;
        for k in 0..count(hwnd) {
            ok &= set_one(hwnd, k, state, mask);
        }
        return ok;
    }
    if i < 0 || i >= count(hwnd) {
        return false;
    }
    if mask & state & LVIS_SELECTED != 0 && style(hwnd) & LVS_SINGLESEL != 0 {
        for k in selected(hwnd) {
            if k != i {
                set_one(hwnd, k, 0, LVIS_SELECTED);
            }
        }
    }
    if mask & state & LVIS_FOCUSED != 0 {
        let f = with(hwnd, |l| l.focus).unwrap_or(-1);
        if f >= 0 && f != i {
            set_one(hwnd, f, 0, LVIS_FOCUSED);
        }
    }
    set_one(hwnd, i, state, mask)
}

fn selected(hwnd: Hwnd) -> Vec<i32> {
    let od = owner_data(hwnd);
    with(hwnd, |l| {
        if od {
            l.virtual_sel.iter().copied().collect()
        } else {
            l.items.iter().enumerate().filter(|(_, it)| it.state & LVIS_SELECTED != 0).map(|(i, _)| i as i32).collect()
        }
    })
    .unwrap_or_default()
}

// -----| reading and writing LVITEM |-----

struct LvItem {
    mask: u32,
    item: i32,
    sub: i32,
    state: u32,
    state_mask: u32,
    text: u64,
    cap: i32,
    image: i32,
    param: i64,
    indent: i32,
}

fn read_lvitem(p: *const u8) -> LvItem {
    unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const i32);
        LvItem {
            mask: d(0) as u32,
            item: d(4),
            sub: d(8),
            state: d(12) as u32,
            state_mask: d(16) as u32,
            text: core::ptr::read_unaligned(p.add(24) as *const u64),
            cap: d(32),
            image: d(36),
            param: core::ptr::read_unaligned(p.add(40) as *const i64),
            indent: d(48),
        }
    }
}

fn text_of(lv: &LvItem, unicode: bool) -> Text {
    if lv.text == TEXT_CALLBACK { Text::Callback } else { Text::Own(crate::text_arg(lv.text, unicode)) }
}

fn insert_item(hwnd: Hwnd, lp: i64, unicode: bool) -> i64 {
    if lp == 0 || owner_data(hwnd) {
        return -1;
    }
    let lv = read_lvitem(lp as *const u8);
    if lv.sub != 0 {
        return -1;
    }
    let st = style(hwnd);
    let text = if lv.mask & LVIF_TEXT != 0 { text_of(&lv, unicode) } else { Text::Own(Vec::new()) };
    let item = Item {
        text,
        subs: Vec::new(),
        image: if lv.mask & LVIF_IMAGE != 0 { lv.image } else { 0 },
        param: if lv.mask & LVIF_PARAM != 0 { lv.param } else { 0 },
        state: if lv.mask & LVIF_STATE != 0 { lv.state & lv.state_mask & !(LVIS_SELECTED | LVIS_FOCUSED) } else { 0 },
        indent: if lv.mask & LVIF_INDENT != 0 { lv.indent } else { 0 },
        pos: None,
    };
    let wanted_state = if lv.mask & LVIF_STATE != 0 { lv.state & lv.state_mask & (LVIS_SELECTED | LVIS_FOCUSED) } else { 0 };
    let sorted = st & (LVS_SORTASCENDING | LVS_SORTDESCENDING) != 0;
    let at = with(hwnd, |l| {
        let mut at = lv.item.clamp(0, l.items.len() as i32) as usize;
        if let (true, Text::Own(t)) = (sorted, &item.text) {
            let key = |s: &[u16]| s.iter().map(|&c| crate::lower(c)).collect::<Vec<u16>>();
            let k = key(t);
            at = l.items.iter().position(|o| {
                let Text::Own(ot) = &o.text else { return false };
                let ok = key(ot);
                if st & LVS_SORTASCENDING != 0 { ok > k } else { ok < k }
            }).unwrap_or(l.items.len());
        }
        l.items.insert(at, item);
        // Everything after moved down.
        if l.focus >= at as i32 {
            l.focus += 1;
        }
        if l.mark >= at as i32 {
            l.mark += 1;
        }
        at as i32
    })
    .unwrap_or(-1);
    if at >= 0 {
        nmlistview(hwnd, LVN_INSERTITEM, at, 0, 0, 0, 0, (0, 0), 0);
        if wanted_state != 0 {
            set_state(hwnd, at, wanted_state, wanted_state);
        }
    }
    layout(hwnd);
    invalidate(hwnd);
    at as i64
}

fn set_item(hwnd: Hwnd, lv: &LvItem, unicode: bool) -> bool {
    let i = lv.item;
    if i < 0 || i >= count(hwnd) {
        return false;
    }
    if owner_data(hwnd) {
        if lv.mask & LVIF_STATE != 0 {
            set_state(hwnd, i, lv.state, lv.state_mask);
        }
        return true;
    }
    let new_text = (lv.mask & LVIF_TEXT != 0).then(|| text_of(lv, unicode));
    if lv.sub != 0 {
        // A subitem has only text.
        let Some(t) = new_text else { return true };
        with(hwnd, |l| {
            let it = &mut l.items[i as usize];
            let k = lv.sub as usize - 1;
            if it.subs.len() <= k {
                it.subs.resize(k + 1, None);
            }
            it.subs[k] = Some(t);
        });
        invalidate(hwnd);
        return true;
    }
    let param = item_param(hwnd, i);
    let mut changed = 0;
    if new_text.is_some() {
        changed |= LVIF_TEXT;
    }
    if lv.mask & LVIF_IMAGE != 0 {
        changed |= LVIF_IMAGE;
    }
    if lv.mask & LVIF_PARAM != 0 {
        changed |= LVIF_PARAM;
    }
    if changed != 0 && nmlistview(hwnd, LVN_ITEMCHANGING, i, 0, 0, 0, changed, (0, 0), param) != 0 {
        return false;
    }
    with(hwnd, |l| {
        let it = &mut l.items[i as usize];
        if let Some(t) = new_text {
            it.text = t;
        }
        if lv.mask & LVIF_IMAGE != 0 {
            it.image = lv.image;
        }
        if lv.mask & LVIF_PARAM != 0 {
            it.param = lv.param;
        }
        if lv.mask & LVIF_INDENT != 0 {
            it.indent = lv.indent;
        }
    });
    if changed != 0 {
        nmlistview(hwnd, LVN_ITEMCHANGED, i, 0, 0, 0, changed, (0, 0), item_param(hwnd, i));
    }
    if lv.mask & LVIF_STATE != 0 {
        set_state(hwnd, i, lv.state, lv.state_mask);
    }
    layout(hwnd);
    invalidate(hwnd);
    true
}

fn get_item(hwnd: Hwnd, lp: i64, unicode: bool) -> bool {
    if lp == 0 {
        return false;
    }
    let p = lp as *mut u8;
    let lv = read_lvitem(p);
    let i = lv.item;
    if i < 0 || i >= count(hwnd) {
        return false;
    }
    let (param, indent) = with(hwnd, |l| l.items.get(i as usize).map_or((0, 0), |it| (it.param, it.indent))).unwrap_or((0, 0));
    if lv.mask & LVIF_TEXT != 0 {
        if lv.mask & LVIF_NORECOMPUTE != 0 && is_callback(hwnd, i, lv.sub) {
            unsafe { core::ptr::write_unaligned(p.add(24) as *mut u64, TEXT_CALLBACK) };
        } else {
            let t = item_text(hwnd, i, lv.sub);
            crate::put_text(lv.text, lv.cap, &t, unicode);
        }
    }
    unsafe {
        if lv.mask & LVIF_IMAGE != 0 {
            core::ptr::write_unaligned(p.add(36) as *mut i32, item_image(hwnd, i));
        }
        if lv.mask & LVIF_PARAM != 0 {
            core::ptr::write_unaligned(p.add(40) as *mut i64, param);
        }
        if lv.mask & LVIF_STATE != 0 {
            core::ptr::write_unaligned(p.add(12) as *mut u32, get_state(hwnd, i) & lv.state_mask);
        }
        if lv.mask & LVIF_INDENT != 0 {
            core::ptr::write_unaligned(p.add(48) as *mut i32, indent);
        }
    }
    true
}

fn is_callback(hwnd: Hwnd, i: i32, sub: i32) -> bool {
    with(hwnd, |l| {
        let Some(it) = l.items.get(i as usize) else { return false };
        if sub == 0 { matches!(it.text, Text::Callback) } else { matches!(it.subs.get(sub as usize - 1), Some(Some(Text::Callback))) }
    })
    .unwrap_or(false)
}

/// Removing an item: after-effects on the focus, the mark and the selection.
fn delete_item(hwnd: Hwnd, i: i32) -> bool {
    if owner_data(hwnd) {
        return with(hwnd, |l| {
            if i < 0 || i >= l.virtual_count {
                return false;
            }
            l.virtual_count -= 1;
            l.virtual_sel = l.virtual_sel.iter().filter(|&&k| k != i).map(|&k| if k > i { k - 1 } else { k }).collect();
            true
        })
        .unwrap_or(false);
    }
    if i < 0 || i >= count(hwnd) {
        return false;
    }
    let param = item_param(hwnd, i);
    nmlistview(hwnd, LVN_ITEMCHANGING, i, 0, 0, get_state(hwnd, i), LVIF_STATE, (0, 0), param);
    nmlistview(hwnd, LVN_DELETEITEM, i, 0, 0, 0, 0, (0, 0), param);
    with(hwnd, |l| {
        if (i as usize) < l.items.len() {
            l.items.remove(i as usize);
        }
        if l.focus == i {
            l.focus = -1;
        } else if l.focus > i {
            l.focus -= 1;
        }
        if l.mark == i {
            l.mark = -1;
        } else if l.mark > i {
            l.mark -= 1;
        }
    });
    layout(hwnd);
    invalidate(hwnd);
    true
}

// -----| columns |-----

fn read_column(lp: i64, unicode: bool, c: &mut Column) -> u32 {
    let p = lp as *const u8;
    unsafe {
        let d = |o: usize| core::ptr::read_unaligned(p.add(o) as *const i32);
        let mask = d(0) as u32;
        if mask & LVCF_FMT != 0 {
            c.fmt = d(4);
        }
        if mask & LVCF_WIDTH != 0 {
            c.width = d(8);
        }
        if mask & LVCF_TEXT != 0 {
            c.text = crate::text_arg(core::ptr::read_unaligned(p.add(16) as *const u64), unicode);
        }
        if mask & LVCF_SUBITEM != 0 {
            c.sub = d(28);
            c.given_sub = d(28);
        }
        if mask & LVCF_IMAGE != 0 {
            c.image = d(32);
        }
        if mask & LVCF_ORDER != 0 {
            c.order = d(36);
        }
        mask
    }
}

/// Header's HDITEM from a column.
fn header_item(c: &Column) -> ([u8; 72], Vec<u16>) {
    let mut t = c.text.clone();
    t.push(0);
    let mut hd = [0u8; 72];
    hd[0..4].copy_from_slice(&(0x1u32 | 0x2 | 0x4 | 0x20).to_le_bytes()); // WIDTH, TEXT, FORMAT, IMAGE
    hd[4..8].copy_from_slice(&c.width.to_le_bytes());
    hd[8..16].copy_from_slice(&(t.as_ptr() as u64).to_le_bytes());
    hd[28..32].copy_from_slice(&(c.fmt & 3).to_le_bytes());
    hd[40..44].copy_from_slice(&c.image.to_le_bytes());
    (hd, t)
}

fn ensure_header(hwnd: Hwnd) {
    if with(hwnd, |l| l.header).unwrap_or(1) != 0 {
        return;
    }
    let class = crate::wide("SysHeader32");
    let st = style(hwnd);
    // WS_CHILD | HDS_HORZ, buttons unless the columns aren't clickable.
    let hs = 0x4000_0000 | if st & LVS_NOSORTHEADER == 0 { 2u32 } else { 0 };
    let h = unsafe { crate::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), hs, 0, 0, 0, 0, hwnd, 0, 0, 0) };
    let font = with(hwnd, |l| {
        l.header = h;
        l.font
    })
    .unwrap_or(0);
    if font != 0 {
        unsafe { crate::SendMessageW(h, 0x0030, font, 0) };
    }
}

fn insert_column(hwnd: Hwnd, i: i32, lp: i64, unicode: bool) -> i64 {
    if lp == 0 {
        return -1;
    }
    ensure_header(hwnd);
    let mut c = Column { text: Vec::new(), width: 0, fmt: 0, sub: i, given_sub: 0, image: 0, order: -1 };
    read_column(lp, unicode, &mut c);
    let (at, header) = with(hwnd, |l| {
        let at = i.clamp(0, l.columns.len() as i32) as usize;
        let order = if c.order >= 0 && (c.order as usize) <= l.columns.len() { c.order } else { at as i32 };
        for o in l.columns.iter_mut() {
            if o.order >= order {
                o.order += 1;
            }
        }
        c.order = order;
        l.columns.insert(at, c.clone());
        (at, l.header)
    })
    .unwrap_or((0, 0));
    let (hd, _t) = header_item(&c);
    unsafe { crate::SendMessageW(header, HDM_INSERTITEMW, at as u64, hd.as_ptr() as i64) };
    layout(hwnd);
    invalidate(hwnd);
    at as i64
}

fn set_column_width(hwnd: Hwnd, i: i32, width: i32) -> bool {
    // LVSCW_AUTOSIZE (-1): the longest cell; LVSCW_AUTOSIZE_USEHEADER (-2):
    // the header too (the last column takes what's left).
    let v = view(hwnd);
    let w = match width {
        -1 | -2 if v == LVS_REPORT => {
            let dc = unsafe { crate::GetDC(hwnd) };
            let old = unsafe { crate::SelectObject(dc, crate::font_or_default(with(hwnd, |l| l.font).unwrap_or(0))) };
            let sub = with(hwnd, |l| l.columns.get(i as usize).map_or(i, |c| c.sub)).unwrap_or(i);
            let mut w = 0;
            for k in 0..count(hwnd).min(512) {
                let t = item_text(hwnd, k, sub);
                w = w.max(crate::text_size(dc, &t).0);
            }
            w += 12;
            if sub == 0 {
                w += small_size(hwnd).0;
            }
            if width == -2 {
                let title = with(hwnd, |l| l.columns.get(i as usize).map(|c| c.text.clone())).flatten().unwrap_or_default();
                w = w.max(crate::text_size(dc, &title).0 + 12);
                let n = with(hwnd, |l| l.columns.len()).unwrap_or(0);
                if i as usize + 1 == n {
                    let others: i32 = with(hwnd, |l| l.columns.iter().enumerate().filter(|(k, _)| *k != i as usize).map(|(_, c)| c.width).sum()).unwrap_or(0);
                    w = w.max(client(hwnd)[2] - others);
                }
            }
            unsafe {
                crate::SelectObject(dc, old);
                crate::ReleaseDC(hwnd, dc);
            }
            w
        }
        _ => width,
    };
    if v == LVS_LIST {
        // The list view's one column width.
        with(hwnd, |l| {
            if l.columns.is_empty() {
                l.columns.push(Column { text: Vec::new(), width: w, fmt: 0, sub: 0, given_sub: 0, image: 0, order: 0 });
            } else {
                l.columns[0].width = w;
            }
        });
    } else {
        let Some(Some(c)) = with(hwnd, |l| {
            let c = l.columns.get_mut(i as usize)?;
            c.width = w;
            Some(c.clone())
        }) else {
            return false;
        };
        let header = with(hwnd, |l| l.header).unwrap_or(0);
        let (hd, _t) = header_item(&c);
        unsafe { crate::SendMessageW(header, HDM_SETITEMW, i as u64, hd.as_ptr() as i64) };
    }
    layout(hwnd);
    invalidate(hwnd);
    true
}

// -----| hit testing |-----

/// The item at client (x, y) and the part hit.
fn hit(hwnd: Hwnd, x: i32, y: i32) -> (i32, u32, i32) {
    let top = header_height(hwnd);
    if y < top {
        return (-1, LVHT_NOWHERE, -1);
    }
    let n = count(hwnd);
    let v = view(hwnd);
    let (sx, sy) = with(hwnd, |l| (l.sx, l.sy)).unwrap_or((0, 0));
    let candidate = match v {
        LVS_REPORT => {
            let i = (y - top + sy) / row_height(hwnd);
            (i < n).then_some(i)
        }
        LVS_LIST => {
            let h = row_height(hwnd);
            let per = (client(hwnd)[3] / h).max(1);
            let w = cell_width(hwnd).max(1);
            let (col, row) = ((x + sx) / w, (y + sy) / h);
            let i = col * per + row;
            (row < per && i < n).then_some(i)
        }
        _ => (0..n.min(4096)).find(|&i| {
            let r = item_rect(hwnd, i);
            x >= r[0] && x < r[2] && y >= r[1] && y < r[3]
        }),
    };
    let Some(i) = candidate else { return (-1, LVHT_NOWHERE, -1) };
    let (icon, label) = parts(hwnd, i);
    let inside = |r: [i32; 4]| x >= r[0] && x < r[2] && y >= r[1] && y < r[3];
    let ex = with(hwnd, |l| l.ex).unwrap_or(0);
    let mut sub = 0;
    if v == LVS_REPORT {
        let rx = x + sx;
        if let Some(c) = column_layout(hwnd).iter().find(|c| rx >= c.1 && rx < c.1 + c.2) {
            sub = with(hwnd, |l| l.columns[c.0].sub).unwrap_or(0);
        } else {
            return (-1, LVHT_NOWHERE, -1);
        }
    }
    let part = if ex & LVS_EX_CHECKBOXES != 0 && x >= icon[0] - 16 && x < icon[0] {
        LVHT_ONITEMSTATEICON
    } else if inside(icon) && icon[2] > icon[0] {
        LVHT_ONITEMICON
    } else if inside(label) || sub != 0 || ex & LVS_EX_FULLROWSELECT != 0 {
        LVHT_ONITEMLABEL
    } else {
        LVHT_NOWHERE
    };
    if part == LVHT_NOWHERE && v != LVS_REPORT {
        return (-1, LVHT_NOWHERE, -1);
    }
    (i, part, sub)
}

// -----| painting |-----

fn draw_text(hdc: u64, text: &[u16], r: [i32; 4], fmt: i32, wrap: bool) {
    let mut t = text.to_vec();
    if t.is_empty() {
        return;
    }
    let mut rr = [r[0] + 2, r[1], r[2] - 2, r[3]];
    let align = match fmt & 3 {
        1 => 0x2,
        2 => 0x1,
        _ => 0,
    };
    // DT_NOPREFIX | DT_END_ELLIPSIS, one line centred, or wrapped and centred.
    let flags = if wrap { 0x800 | 0x10 | 0x1 } else { 0x800 | 0x8000 | 0x20 | 0x4 | align };
    unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, flags) };
}

fn paint(hwnd: Hwnd, hdc: u64) {
    let c = client(hwnd);
    let Some((font, ex, images, bk, text_color, text_bk, has_focus, sx)) =
        with(hwnd, |l| (l.font, l.ex, l.images, l.bk, l.text_color, l.text_bk, l.has_focus, l.sx))
    else {
        return;
    };
    let bg = |hdc: u64, r: &[i32; 4], color: u32| unsafe {
        if color == CLR_NONE {
            return;
        }
        if color == CLR_DEFAULT {
            crate::FillRect(hdc, r, crate::GetSysColorBrush(5));
        } else {
            let b = crate::CreateSolidBrush(color);
            crate::FillRect(hdc, r, b);
            crate::DeleteObject(b);
        }
    };
    bg(hdc, &c, bk);
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    let v = view(hwnd);
    let st = style(hwnd);
    let n = count(hwnd);
    let top = header_height(hwnd);
    let normal_color = if text_color == CLR_DEFAULT { unsafe { crate::GetSysColor(8) } } else { text_color };
    let il = if v == 0 { images[0] } else { images[1] };
    let (first, last) = if v == LVS_REPORT {
        let h = row_height(hwnd);
        let sy = with(hwnd, |l| l.sy).unwrap_or(0);
        (sy / h, ((sy + (c[3] - top)) / h + 1).min(n))
    } else {
        (0, n)
    };
    let lay = column_layout(hwnd);
    for i in first..last {
        let r = item_rect(hwnd, i);
        if r[3] <= top || r[1] >= c[3] || r[2] <= 0 || r[0] >= c[2] {
            continue;
        }
        let state = get_state(hwnd, i);
        let (mut icon, label) = parts(hwnd, i);
        let sel = state & LVIS_SELECTED != 0 && (has_focus || st & LVS_SHOWSELALWAYS != 0);
        let (sel_bg, sel_fg) = if has_focus { (13, 14) } else { (15, 18) };
        let full = v == LVS_REPORT && ex & LVS_EX_FULLROWSELECT != 0;
        if sel && full {
            unsafe { crate::FillRect(hdc, &[r[0], r[1], r[2].max(c[2]), r[3]], crate::GetSysColorBrush(sel_bg)) };
        } else if text_bk != CLR_NONE && text_bk != CLR_DEFAULT {
            bg(hdc, &label, text_bk);
        }
        if ex & LVS_EX_CHECKBOXES != 0 || images[2] != 0 {
            let box_x = icon[0];
            icon[0] += 16;
            icon[2] += 16;
            let image = ((state & LVIS_STATEIMAGEMASK) >> 12) as i32 - 1;
            if images[2] != 0 && image >= 0 {
                crate::imagelist::ImageList_Draw(images[2], image, hdc, box_x, r[1], 0);
            } else if ex & LVS_EX_CHECKBOXES != 0 {
                let b = [box_x + 1, r[1] + (r[3] - r[1] - 13) / 2, box_x + 14, r[1] + (r[3] - r[1] - 13) / 2 + 13];
                unsafe {
                    crate::FillRect(hdc, &b, crate::GetSysColorBrush(5));
                    crate::FillRect(hdc, &[b[0], b[1], b[2], b[1] + 1], crate::GetSysColorBrush(16));
                    crate::FillRect(hdc, &[b[0], b[3] - 1, b[2], b[3]], crate::GetSysColorBrush(16));
                    crate::FillRect(hdc, &[b[0], b[1], b[0] + 1, b[3]], crate::GetSysColorBrush(16));
                    crate::FillRect(hdc, &[b[2] - 1, b[1], b[2], b[3]], crate::GetSysColorBrush(16));
                    if image == 1 {
                        crate::FillRect(hdc, &[b[0] + 3, b[1] + 3, b[2] - 3, b[3] - 3], crate::GetSysColorBrush(8));
                    }
                }
            }
        }
        if il != 0 {
            let image = item_image(hwnd, i);
            if image >= 0 {
                // ILD_SELECTED for a selected image is left out.
                let y = if v == 0 { icon[1] } else { icon[1] + (icon[3] - icon[1] - small_size(hwnd).1) / 2 };
                let overlay = state & 0x0F00;
                crate::imagelist::ImageList_Draw(il, image, hdc, icon[0], y, overlay);
            }
        }
        if v == LVS_REPORT {
            for (k, x, w) in lay.iter().copied() {
                let (sub, fmt) = with(hwnd, |l| (l.columns[k].sub, l.columns[k].fmt)).unwrap_or((0, 0));
                let (mut cr, base) = ([x - sx, r[1], x - sx + w, r[3]], sub == 0);
                if base {
                    cr[0] = label[0] + if ex & LVS_EX_CHECKBOXES != 0 || images[2] != 0 { 16 } else { 0 };
                }
                if cr[2] <= 0 || cr[0] >= c[2] {
                    continue;
                }
                let t = item_text(hwnd, i, sub);
                let on = sel && (full || base);
                if on && !full {
                    let tw = crate::text_size(hdc, &t).0 + 6;
                    unsafe { crate::FillRect(hdc, &[cr[0], cr[1], (cr[0] + tw).min(cr[2]), cr[3]], crate::GetSysColorBrush(sel_bg)) };
                }
                unsafe { crate::SetTextColor(hdc, if on { crate::GetSysColor(sel_fg) } else { normal_color }) };
                draw_text(hdc, &t, cr, if base { 0 } else { fmt }, false);
            }
        } else {
            let t = item_text(hwnd, i, 0);
            let mut lr = label;
            if v != 0 {
                lr[0] = icon[2].max(lr[0]);
                lr[2] = lr[2].min(lr[0] + crate::text_size(hdc, &t).0 + 6);
            }
            if sel {
                unsafe { crate::FillRect(hdc, &lr, crate::GetSysColorBrush(sel_bg)) };
            }
            unsafe { crate::SetTextColor(hdc, if sel { crate::GetSysColor(sel_fg) } else { normal_color }) };
            draw_text(hdc, &t, lr, 0, v == 0);
        }
        if state & LVIS_FOCUSED != 0 && has_focus {
            let fr = if full || v != LVS_REPORT { [r[0], r[1], r[2], r[3]] } else { [label[0], r[1], label[2], r[3]] };
            unsafe { crate::DrawFocusRect(hdc, &fr) };
        }
        if v == LVS_REPORT && ex & LVS_EX_GRIDLINES != 0 {
            unsafe { crate::FillRect(hdc, &[r[0], r[3] - 1, c[2], r[3]], crate::GetSysColorBrush(15)) };
        }
    }
    if v == LVS_REPORT && ex & LVS_EX_GRIDLINES != 0 {
        for (_, x, w) in lay.iter().copied() {
            let at = x + w - sx - 1;
            unsafe { crate::FillRect(hdc, &[at, top, at + 1, c[3]], crate::GetSysColorBrush(15)) };
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

// -----| scrolling and the keyboard |-----

fn ensure_visible(hwnd: Hwnd, i: i32) {
    if i < 0 || i >= count(hwnd) {
        return;
    }
    let c = client(hwnd);
    let top = header_height(hwnd);
    let r = view_rect(hwnd, i);
    with(hwnd, |l| {
        if r[1] < l.sy {
            l.sy = r[1];
        } else if r[3] > l.sy + (c[3] - top) {
            l.sy = r[3] - (c[3] - top);
        }
        if r[0] < l.sx {
            l.sx = r[0];
        } else if r[2] > l.sx + c[2] && r[2] - r[0] <= c[2] {
            l.sx = r[2] - c[2];
        }
    });
    layout(hwnd);
    invalidate(hwnd);
}

/// Move the focus to `to`, selecting it (Ctrl only moves the focus; Shift
/// selects from the mark).
fn move_to(hwnd: Hwnd, to: i32) {
    let n = count(hwnd);
    if n == 0 {
        return;
    }
    let to = to.clamp(0, n - 1);
    let shift = unsafe { crate::GetKeyState(0x10) } < 0;
    let ctrl = unsafe { crate::GetKeyState(0x11) } < 0;
    let single = style(hwnd) & LVS_SINGLESEL != 0;
    if ctrl && !single {
        set_state(hwnd, to, LVIS_FOCUSED, LVIS_FOCUSED);
    } else if shift && !single {
        let mark = with(hwnd, |l| l.mark).unwrap_or(-1).max(0);
        let (lo, hi) = (mark.min(to), mark.max(to));
        for k in selected(hwnd) {
            if k < lo || k > hi {
                set_one(hwnd, k, 0, LVIS_SELECTED);
            }
        }
        for k in lo..=hi {
            set_one(hwnd, k, LVIS_SELECTED, LVIS_SELECTED);
        }
        set_state(hwnd, to, LVIS_FOCUSED, LVIS_FOCUSED);
    } else {
        for k in selected(hwnd) {
            if k != to {
                set_one(hwnd, k, 0, LVIS_SELECTED);
            }
        }
        set_state(hwnd, to, LVIS_SELECTED | LVIS_FOCUSED, LVIS_SELECTED | LVIS_FOCUSED);
        with(hwnd, |l| l.mark = to);
    }
    ensure_visible(hwnd, to);
}

fn next_item(hwnd: Hwnd, start: i32, flags: u32) -> i32 {
    let n = count(hwnd);
    let dir = flags & (LVNI_ABOVE | LVNI_BELOW | LVNI_TOLEFT | LVNI_TORIGHT);
    let want = flags & (LVNI_FOCUSED | LVNI_SELECTED | LVNI_CUT | LVNI_DROPHILITED);
    let ok = |i: i32| {
        let s = get_state(hwnd, i);
        (want & LVNI_FOCUSED == 0 || s & 1 != 0) && (want & LVNI_SELECTED == 0 || s & 2 != 0) && (want & LVNI_CUT == 0 || s & 4 != 0) && (want & LVNI_DROPHILITED == 0 || s & 8 != 0)
    };
    if dir != 0 {
        if start < 0 {
            return -1;
        }
        let r = view_rect(hwnd, start);
        let (cx, cy) = ((r[0] + r[2]) / 2, (r[1] + r[3]) / 2);
        let mut best: Option<(i32, i32)> = None;
        for i in 0..n {
            if i == start || !ok(i) {
                continue;
            }
            let o = view_rect(hwnd, i);
            let (ox, oy) = ((o[0] + o[2]) / 2, (o[1] + o[3]) / 2);
            let good = match dir {
                LVNI_ABOVE => o[3] <= r[1] && cx >= o[0] && cx < o[2],
                LVNI_BELOW => o[1] >= r[3] && cx >= o[0] && cx < o[2],
                LVNI_TOLEFT => o[2] <= r[0] && cy >= o[1] && cy < o[3],
                _ => o[0] >= r[2] && cy >= o[1] && cy < o[3],
            };
            if good {
                let d = (ox - cx).abs() + (oy - cy).abs();
                if best.is_none_or(|b| d < b.1) {
                    best = Some((i, d));
                }
            }
        }
        return best.map_or(-1, |b| b.0);
    }
    (start + 1..n).find(|&i| ok(i)).unwrap_or(-1)
}

fn key_down(hwnd: Hwnd, vk: u32) {
    let mut e = [0u8; 8];
    e[0..2].copy_from_slice(&(vk as u16).to_le_bytes());
    crate::notify(hwnd, LVN_KEYDOWN, &e);
    let f = with(hwnd, |l| l.focus).unwrap_or(-1);
    let n = count(hwnd);
    let page = (client(hwnd)[3] - header_height(hwnd)) / row_height(hwnd).max(1);
    let v = view(hwnd);
    let move_dir = |flag: u32, step: i32| {
        if f < 0 {
            move_to(hwnd, 0);
        } else if v == LVS_REPORT {
            move_to(hwnd, f + step);
        } else {
            let o = next_item(hwnd, f, flag);
            if o >= 0 {
                move_to(hwnd, o);
            }
        }
    };
    match vk {
        0x26 => move_dir(LVNI_ABOVE, -1),
        0x28 => move_dir(LVNI_BELOW, 1),
        0x25 if v != LVS_REPORT => move_dir(LVNI_TOLEFT, -1),
        0x27 if v != LVS_REPORT => move_dir(LVNI_TORIGHT, 1),
        0x25 | 0x27 => {
            let d = if vk == 0x25 { -20 } else { 20 };
            with(hwnd, |l| l.sx += d);
            layout(hwnd);
            invalidate(hwnd);
        }
        0x24 => move_to(hwnd, 0),
        0x23 => move_to(hwnd, n - 1),
        0x21 => move_to(hwnd, f.max(0) - page.max(1)),
        0x22 => move_to(hwnd, f.max(0) + page.max(1)),
        0x41 if unsafe { crate::GetKeyState(0x11) } < 0 && style(hwnd) & LVS_SINGLESEL == 0 => {
            set_state(hwnd, -1, LVIS_SELECTED, LVIS_SELECTED);
        }
        0x20 if with(hwnd, |l| l.ex).unwrap_or(0) & LVS_EX_CHECKBOXES != 0 && f >= 0 => toggle_check(hwnd, f),
        0x71 if style(hwnd) & LVS_EDITLABELS != 0 && f >= 0 => {
            start_edit(hwnd, f);
        }
        0x0D => {
            crate::notify(hwnd, NM_RETURN, &[]);
            if f >= 0 {
                activate(hwnd, f, 0, (0, 0));
            }
        }
        _ => {}
    }
}

fn toggle_check(hwnd: Hwnd, i: i32) {
    let s = get_state(hwnd, i);
    let next = if (s & LVIS_STATEIMAGEMASK) >> 12 == 2 { 1 } else { 2 };
    set_state(hwnd, i, next << 12, LVIS_STATEIMAGEMASK);
}

/// NMITEMACTIVATE after the NMHDR.
fn nmactivate(hwnd: Hwnd, code: i32, item: i32, sub: i32, pt: (i32, i32)) -> i64 {
    let mut e = [0u8; 48];
    e[0..4].copy_from_slice(&item.to_le_bytes());
    e[4..8].copy_from_slice(&sub.to_le_bytes());
    e[20..24].copy_from_slice(&pt.0.to_le_bytes());
    e[24..28].copy_from_slice(&pt.1.to_le_bytes());
    e[32..40].copy_from_slice(&item_param(hwnd, item).to_le_bytes());
    crate::notify(hwnd, code, &e)
}

fn activate(hwnd: Hwnd, item: i32, sub: i32, pt: (i32, i32)) {
    nmactivate(hwnd, LVN_ITEMACTIVATE, item, sub, pt);
}

// -----| label editing |-----

/// NMLVDISPINFO with `text`.
fn label_notify(hwnd: Hwnd, i: i32, code_w: i32, code_a: i32, text: Option<&[u16]>) -> i64 {
    let unicode = crate::parent_unicode(hwnd);
    let mut narrow = alloc::vec![0u8; 1];
    let mut w: Vec<u16> = text.map_or(Vec::new(), |t| t.to_vec());
    w.push(0);
    let ptr: u64 = match (text.is_some(), unicode) {
        (false, _) => 0,
        (true, true) => w.as_ptr() as u64,
        (true, false) => {
            narrow = alloc::vec![0u8; w.len() * 2 + 1];
            crate::put_text(narrow.as_mut_ptr() as u64, narrow.len() as i32, &w[..w.len() - 1], false);
            narrow.as_ptr() as u64
        }
    };
    let _ = &narrow;
    let mut lvi = [0u8; 88];
    lvi[0..4].copy_from_slice(&LVIF_TEXT.to_le_bytes());
    lvi[4..8].copy_from_slice(&i.to_le_bytes());
    lvi[24..32].copy_from_slice(&ptr.to_le_bytes());
    lvi[40..48].copy_from_slice(&item_param(hwnd, i).to_le_bytes());
    crate::notify(hwnd, if unicode { code_w } else { code_a }, &lvi)
}

fn start_edit(hwnd: Hwnd, i: i32) -> Hwnd {
    if i < 0 || i >= count(hwnd) || style(hwnd) & LVS_EDITLABELS == 0 {
        return 0;
    }
    let text = item_text(hwnd, i, 0);
    if label_notify(hwnd, i, LVN_BEGINLABELEDITW, LVN_BEGINLABELEDITA, Some(&text)) != 0 {
        return 0;
    }
    ensure_visible(hwnd, i);
    let (_, mut r) = parts(hwnd, i);
    r[1] = r[1].min(item_rect(hwnd, i)[1]);
    r[3] = r[1] + line_height(hwnd) + 4;
    let font = with(hwnd, |l| l.font).unwrap_or(0);
    let e = crate::labeledit::start(hwnd, r, &text, font);
    with(hwnd, |l| {
        l.edit = e;
        l.edit_item = i;
    });
    e
}

fn end_edit(hwnd: Hwnd, keep: bool) {
    let Some((e, i)) = with(hwnd, |l| (core::mem::replace(&mut l.edit, 0), l.edit_item)) else { return };
    if e == 0 {
        return;
    }
    let t = crate::labeledit::text(e);
    let r = label_notify(hwnd, i, LVN_ENDLABELEDITW, LVN_ENDLABELEDITA, if keep { Some(&t) } else { None });
    if keep && r != 0 {
        with(hwnd, |l| {
            if let Some(it) = l.items.get_mut(i as usize) {
                if !matches!(it.text, Text::Callback) {
                    it.text = Text::Own(t);
                }
            }
        });
    }
    unsafe {
        crate::DestroyWindow(e);
        crate::SetFocus(hwnd);
    }
    invalidate(hwnd);
}

// -----| sorting |-----

fn sort(hwnd: Hwnd, cmp: usize, lp: i64, by_index: bool) -> bool {
    if cmp == 0 || owner_data(hwnd) {
        return false;
    }
    let f: unsafe extern "system" fn(i64, i64, i64) -> i32 = unsafe { core::mem::transmute(cmp) };
    let items: Vec<Item> = with(hwnd, |l| l.items.clone()).unwrap_or_default();
    let mut order: Vec<usize> = (0..items.len()).collect();
    if by_index {
        crate::merge_sort(&mut order, &mut |a, b| unsafe { f(*a as i64, *b as i64, lp) });
    } else {
        crate::merge_sort(&mut order, &mut |a, b| unsafe { f(items[*a].param, items[*b].param, lp) });
    }
    with(hwnd, |l| {
        l.items = order.iter().map(|&i| items[i].clone()).collect();
        if let Some(k) = l.items.iter().position(|it| it.state & LVIS_FOCUSED != 0) {
            l.focus = k as i32;
        }
    });
    invalidate(hwnd);
    true
}

fn find_item(hwnd: Hwnd, start: i32, info: i64, unicode: bool) -> i64 {
    if info == 0 {
        return -1;
    }
    let p = info as *const u8;
    let (flags, psz, param) = unsafe {
        (
            core::ptr::read_unaligned(p as *const u32),
            core::ptr::read_unaligned(p.add(8) as *const u64),
            core::ptr::read_unaligned(p.add(16) as *const i64),
        )
    };
    let n = count(hwnd);
    let from = (start + 1).clamp(0, n);
    let needle: Vec<u16> = if flags & (LVFI_STRING | LVFI_PARTIAL) != 0 {
        crate::text_arg(psz, unicode).iter().map(|&c| crate::lower(c)).collect()
    } else {
        Vec::new()
    };
    let matches = |i: i32| {
        if flags & LVFI_PARAM != 0 {
            return item_param(hwnd, i) == param;
        }
        if flags & (LVFI_STRING | LVFI_PARTIAL) != 0 {
            let t: Vec<u16> = item_text(hwnd, i, 0).iter().map(|&c| crate::lower(c)).collect();
            return if flags & LVFI_PARTIAL != 0 { t.starts_with(&needle) } else { t == needle };
        }
        false
    };
    if let Some(i) = (from..n).find(|&i| matches(i)) {
        return i as i64;
    }
    if flags & LVFI_WRAP != 0 {
        if let Some(i) = (0..from).find(|&i| matches(i)) {
            return i as i64;
        }
    }
    -1
}

// -----| the window procedure |-----

unsafe extern "system" fn list_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    let def = || unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        0x0081 => {
            LISTS.lock().insert(
                hwnd,
                List {
                    items: Vec::new(),
                    columns: Vec::new(),
                    header: 0,
                    font: 0,
                    ex: 0,
                    images: [0; 3],
                    bk: CLR_DEFAULT,
                    text_color: CLR_DEFAULT,
                    text_bk: CLR_DEFAULT,
                    sx: 0,
                    sy: 0,
                    focus: -1,
                    mark: -1,
                    callback_mask: 0,
                    virtual_count: 0,
                    virtual_sel: BTreeSet::new(),
                    edit: 0,
                    edit_item: -1,
                    has_focus: false,
                    icon_spacing: (75, 75),
                },
            );
            def()
        }
        0x0001 => {
            if view(hwnd) == LVS_REPORT {
                ensure_header(hwnd);
            }
            layout(hwnd);
            0
        }
        0x0002 => {
            // Each item says goodbye (LVN_DELETEALLITEMS can spare that).
            if count(hwnd) > 0 && !owner_data(hwnd) {
                delete_all(hwnd);
            }
            0
        }
        0x0082 => {
            LISTS.lock().remove(&hwnd);
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
        // WM_STYLECHANGED: the view may have changed.
        0x007D => {
            if view(hwnd) == LVS_REPORT {
                ensure_header(hwnd);
            }
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0030 => {
            let h = with(hwnd, |l| {
                l.font = wp;
                l.header
            })
            .unwrap_or(0);
            if h != 0 {
                unsafe { crate::SendMessageW(h, 0x0030, wp, lp) };
            }
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0031 => with(hwnd, |l| l.font as i64).unwrap_or(0),
        0x0007 => {
            with(hwnd, |l| l.has_focus = true);
            crate::notify(hwnd, NM_SETFOCUS, &[]);
            invalidate(hwnd);
            0
        }
        0x0008 => {
            with(hwnd, |l| l.has_focus = false);
            crate::notify(hwnd, NM_KILLFOCUS, &[]);
            invalidate(hwnd);
            0
        }
        0x0087 => 0x1 | 0x4 | 0x80, // DLGC_WANTARROWS | DLGC_WANTCHARS | ...
        0x0100 => {
            key_down(hwnd, wp as u32);
            0
        }
        crate::labeledit::DONE => {
            end_edit(hwnd, wp != 0);
            0
        }
        WM_VSCROLL | WM_HSCROLL | 0x020A => {
            let c = client(hwnd);
            let n = count(hwnd);
            let top = header_height(hwnd);
            if msg == WM_VSCROLL || msg == 0x020A {
                let line = row_height(hwnd);
                let page = c[3] - top;
                let content = if view(hwnd) == LVS_REPORT { n * line } else if n > 0 { view_rect(hwnd, n - 1)[3] } else { 0 };
                let pos = with(hwnd, |l| l.sy).unwrap_or(0);
                let target = if msg == 0x020A {
                    let delta = ((wp >> 16) & 0xFFFF) as i16 as i32;
                    (pos - delta * 3 * line / 120).clamp(0, (content - page).max(0))
                } else {
                    crate::scroll_target(hwnd, 1, wp, line, page, pos, (content - page).max(0))
                };
                with(hwnd, |l| l.sy = target);
            } else {
                let cw = if view(hwnd) == LVS_LIST { cell_width(hwnd) } else { 20 };
                let pos = with(hwnd, |l| l.sx).unwrap_or(0);
                let content = if view(hwnd) == LVS_REPORT {
                    columns_width(hwnd)
                } else {
                    let per = (c[3] / row_height(hwnd)).max(1);
                    (n + per - 1) / per * cell_width(hwnd)
                };
                let target = crate::scroll_target(hwnd, 0, wp, cw, c[2], pos, (content - c[2]).max(0));
                with(hwnd, |l| l.sx = target);
            }
            layout(hwnd);
            invalidate(hwnd);
            0
        }
        0x0201 | 0x0203 | 0x0204 | 0x0206 => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            unsafe { crate::SetFocus(hwnd) };
            let (i, part, sub) = hit(hwnd, x, y);
            let double = msg == 0x0203 || msg == 0x0206;
            let right = msg == 0x0204 || msg == 0x0206;
            if i >= 0 && part == LVHT_ONITEMSTATEICON && !right {
                toggle_check(hwnd, i);
            } else if i >= 0 {
                if !double {
                    let already = get_state(hwnd, i) & LVIS_SELECTED != 0;
                    if !(right && already) {
                        let ctrl = unsafe { crate::GetKeyState(0x11) } < 0;
                        if ctrl && style(hwnd) & LVS_SINGLESEL == 0 {
                            let s = get_state(hwnd, i);
                            set_state(hwnd, i, (s ^ LVIS_SELECTED) | LVIS_FOCUSED, LVIS_SELECTED | LVIS_FOCUSED);
                            with(hwnd, |l| l.mark = i);
                        } else {
                            move_to(hwnd, i);
                        }
                    }
                    // A second click on the selected item edits its label.
                    if !right && already && style(hwnd) & LVS_EDITLABELS != 0 && part == LVHT_ONITEMLABEL && sub == 0 {
                        start_edit(hwnd, i);
                    }
                }
            } else if !right {
                // Empty space clears the selection.
                for k in selected(hwnd) {
                    set_one(hwnd, k, 0, LVIS_SELECTED);
                }
            }
            let code = match (right, double) {
                (false, false) => NM_CLICK,
                (false, true) => NM_DBLCLK,
                (true, false) => NM_RCLICK,
                (true, true) => -6,
            };
            nmactivate(hwnd, code, i, sub.max(0), (x, y));
            if double && !right && i >= 0 {
                activate(hwnd, i, sub.max(0), (x, y));
            }
            0
        }
        // From the header.
        crate::WM_NOTIFY => {
            if lp == 0 {
                return 0;
            }
            let code = unsafe { *((lp as *const u8).add(16) as *const i32) };
            let from = unsafe { *(lp as *const u64) };
            if from != with(hwnd, |l| l.header).unwrap_or(0) {
                return 0;
            }
            let item = unsafe { *((lp as *const u8).add(24) as *const i32) };
            match code {
                HDN_ITEMCLICKW => {
                    nmlistview(hwnd, LVN_COLUMNCLICK, -1, item, 0, 0, 0, (0, 0), 0);
                }
                HDN_ITEMCHANGEDW => {
                    let hd = unsafe { *((lp as *const u8).add(32) as *const u64) };
                    if hd != 0 {
                        let w = unsafe { core::ptr::read_unaligned((hd as *const u8).add(4) as *const i32) };
                        let mask = unsafe { *(hd as *const u32) };
                        if mask & 1 != 0 {
                            with(hwnd, |l| {
                                if let Some(c) = l.columns.get_mut(item as usize) {
                                    c.width = w;
                                }
                            });
                            layout(hwnd);
                            invalidate(hwnd);
                        }
                    }
                }
                _ => {}
            }
            0
        }
        // The header's parent speaks Unicode to it, whatever our parent does.
        0x0055 => 2,

        LVM_GETBKCOLOR => with(hwnd, |l| if l.bk == CLR_DEFAULT { unsafe { crate::GetSysColor(5) as i64 } } else { l.bk as i64 }).unwrap_or(0),
        LVM_SETBKCOLOR => {
            with(hwnd, |l| l.bk = lp as u32);
            invalidate(hwnd);
            1
        }
        LVM_GETIMAGELIST => with(hwnd, |l| l.images.get(wp as usize).copied().unwrap_or(0) as i64).unwrap_or(0),
        LVM_SETIMAGELIST => {
            let old = with(hwnd, |l| match l.images.get_mut(wp as usize) {
                Some(s) => core::mem::replace(s, lp as u64) as i64,
                None => 0,
            })
            .unwrap_or(0);
            layout(hwnd);
            invalidate(hwnd);
            old
        }
        LVM_GETITEMCOUNT => count(hwnd) as i64,
        LVM_GETITEMW | LVM_GETITEMA => get_item(hwnd, lp, msg == LVM_GETITEMW) as i64,
        LVM_SETITEMW | LVM_SETITEMA => {
            if lp == 0 {
                return 0;
            }
            set_item(hwnd, &read_lvitem(lp as *const u8), msg == LVM_SETITEMW) as i64
        }
        LVM_INSERTITEMW | LVM_INSERTITEMA => insert_item(hwnd, lp, msg == LVM_INSERTITEMW),
        LVM_DELETEITEM => {
            let r = delete_item(hwnd, wp as i32);
            layout(hwnd);
            invalidate(hwnd);
            r as i64
        }
        LVM_DELETEALLITEMS => {
            delete_all(hwnd);
            1
        }
        LVM_GETCALLBACKMASK => with(hwnd, |l| l.callback_mask as i64).unwrap_or(0),
        LVM_SETCALLBACKMASK => {
            with(hwnd, |l| l.callback_mask = wp as u32);
            1
        }
        LVM_GETNEXTITEM => next_item(hwnd, wp as i32, (lp as u32) & 0xFFFF) as i64,
        LVM_FINDITEMW | LVM_FINDITEMA => find_item(hwnd, wp as i32, lp, msg == LVM_FINDITEMW),
        LVM_GETITEMRECT | LVM_GETSUBITEMRECT => {
            if lp == 0 {
                return 0;
            }
            let i = wp as i32;
            if i < 0 || i >= count(hwnd) {
                return 0;
            }
            let rp = lp as *mut [i32; 4];
            let (sub, code) = unsafe { if msg == LVM_GETITEMRECT { (0, (*rp)[0]) } else { ((*rp)[1], (*rp)[0]) } };
            let r = item_rect(hwnd, i);
            let (icon, label) = parts(hwnd, i);
            let out = if msg == LVM_GETSUBITEMRECT && sub > 0 {
                let lay = column_layout(hwnd);
                let col = with(hwnd, |l| l.columns.iter().position(|c| c.sub == sub)).flatten();
                let sx = with(hwnd, |l| l.sx).unwrap_or(0);
                match col.and_then(|k| lay.iter().find(|c| c.0 == k)) {
                    Some(c) => [c.1 - sx, r[1], c.1 + c.2 - sx, r[3]],
                    None => return 0,
                }
            } else {
                match code {
                    LVIR_BOUNDS => r,
                    LVIR_ICON => icon,
                    LVIR_LABEL => label,
                    _ => [icon[0].min(label[0]), r[1], label[2], r[3]],
                }
            };
            unsafe { *rp = out };
            1
        }
        LVM_SETITEMPOSITION | LVM_SETITEMPOSITION32 => {
            let (x, y) = if msg == LVM_SETITEMPOSITION {
                ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
            } else if lp != 0 {
                unsafe { (*(lp as *const i32), *(lp as *const i32).add(1)) }
            } else {
                return 0;
            };
            let ok = with(hwnd, |l| match l.items.get_mut(wp as usize) {
                Some(it) => {
                    it.pos = Some((x, y));
                    true
                }
                None => false,
            })
            .unwrap_or(false);
            invalidate(hwnd);
            ok as i64
        }
        LVM_GETITEMPOSITION => {
            if lp == 0 || wp as i32 >= count(hwnd) {
                return 0;
            }
            let r = item_rect(hwnd, wp as i32);
            unsafe { *(lp as *mut [i32; 2]) = [r[0], r[1]] };
            1
        }
        LVM_GETSTRINGWIDTHW | LVM_GETSTRINGWIDTHA => {
            let t = crate::text_arg(lp as u64, msg == LVM_GETSTRINGWIDTHW);
            let dc = unsafe { crate::GetDC(hwnd) };
            let old = unsafe { crate::SelectObject(dc, crate::font_or_default(with(hwnd, |l| l.font).unwrap_or(0))) };
            let w = crate::text_size(dc, &t).0;
            unsafe {
                crate::SelectObject(dc, old);
                crate::ReleaseDC(hwnd, dc);
            }
            w as i64
        }
        LVM_HITTEST | LVM_SUBITEMHITTEST => {
            if lp == 0 {
                return -1;
            }
            let p = lp as *mut i32;
            let (x, y) = unsafe { (*p, *p.add(1)) };
            let (i, part, sub) = hit(hwnd, x, y);
            unsafe {
                *p.add(2) = part as i32;
                *p.add(3) = i;
                *p.add(4) = sub.max(0);
            }
            i as i64
        }
        LVM_ENSUREVISIBLE => {
            ensure_visible(hwnd, wp as i32);
            1
        }
        LVM_SCROLL => {
            with(hwnd, |l| {
                l.sx += wp as i32;
                l.sy += lp as i32;
            });
            layout(hwnd);
            invalidate(hwnd);
            1
        }
        LVM_REDRAWITEMS | LVM_UPDATE | LVM_ARRANGE => {
            invalidate(hwnd);
            1
        }
        LVM_EDITLABELW | LVM_EDITLABELA => start_edit(hwnd, wp as i32) as i64,
        LVM_GETEDITCONTROL => with(hwnd, |l| l.edit as i64).unwrap_or(0),
        LVM_GETCOLUMNW | LVM_GETCOLUMNA => {
            if lp == 0 {
                return 0;
            }
            let Some(c) = with(hwnd, |l| l.columns.get(wp as usize).cloned()).flatten() else { return 0 };
            let p = lp as *mut u8;
            unsafe {
                let mask = core::ptr::read_unaligned(p as *const u32);
                if mask & LVCF_FMT != 0 {
                    // HDF_STRING comes along from the header.
                    core::ptr::write_unaligned(p.add(4) as *mut i32, c.fmt | 0x4000);
                }
                if mask & LVCF_WIDTH != 0 {
                    core::ptr::write_unaligned(p.add(8) as *mut i32, c.width);
                }
                if mask & LVCF_TEXT != 0 {
                    let dst = core::ptr::read_unaligned(p.add(16) as *const u64);
                    let cap = core::ptr::read_unaligned(p.add(24) as *const i32);
                    crate::put_text(dst, cap, &c.text, msg == LVM_GETCOLUMNW);
                }
                if mask & LVCF_SUBITEM != 0 {
                    core::ptr::write_unaligned(p.add(28) as *mut i32, c.given_sub);
                }
                if mask & LVCF_IMAGE != 0 {
                    core::ptr::write_unaligned(p.add(32) as *mut i32, c.image);
                }
                if mask & LVCF_ORDER != 0 {
                    core::ptr::write_unaligned(p.add(36) as *mut i32, c.order);
                }
            }
            1
        }
        LVM_SETCOLUMNW | LVM_SETCOLUMNA => {
            if lp == 0 {
                return 0;
            }
            let Some(Some(c)) = with(hwnd, |l| {
                let c = l.columns.get_mut(wp as usize)?;
                read_column(lp, msg == LVM_SETCOLUMNW, c);
                Some(c.clone())
            }) else {
                return 0;
            };
            let header = with(hwnd, |l| l.header).unwrap_or(0);
            let (hd, _t) = header_item(&c);
            unsafe { crate::SendMessageW(header, HDM_SETITEMW, wp, hd.as_ptr() as i64) };
            layout(hwnd);
            invalidate(hwnd);
            1
        }
        LVM_INSERTCOLUMNW | LVM_INSERTCOLUMNA => insert_column(hwnd, wp as i32, lp, msg == LVM_INSERTCOLUMNW),
        LVM_DELETECOLUMN => {
            let Some((ok, header)) = with(hwnd, |l| {
                let i = wp as usize;
                if i >= l.columns.len() {
                    return (false, l.header);
                }
                let gone = l.columns.remove(i).order;
                for o in l.columns.iter_mut() {
                    if o.order > gone {
                        o.order -= 1;
                    }
                }
                (true, l.header)
            }) else {
                return 0;
            };
            if ok {
                unsafe { crate::SendMessageW(header, HDM_DELETEITEM, wp, 0) };
                layout(hwnd);
                invalidate(hwnd);
            }
            ok as i64
        }
        LVM_GETCOLUMNWIDTH => {
            if view(hwnd) == LVS_LIST {
                return cell_width(hwnd) as i64;
            }
            with(hwnd, |l| l.columns.get(wp as usize).map_or(0, |c| c.width as i64)).unwrap_or(0)
        }
        LVM_SETCOLUMNWIDTH => set_column_width(hwnd, wp as i32, (lp & 0xFFFF) as i16 as i32) as i64,
        LVM_GETHEADER => {
            ensure_header(hwnd);
            with(hwnd, |l| l.header as i64).unwrap_or(0)
        }
        LVM_GETVIEWRECT => {
            if lp == 0 {
                return 0;
            }
            let n = count(hwnd);
            let mut out = [0, 0, 0, 0];
            if n > 0 {
                let r = view_rect(hwnd, n - 1);
                out = [0, 0, r[2], r[3]];
            }
            unsafe { *(lp as *mut [i32; 4]) = out };
            1
        }
        LVM_GETTEXTCOLOR => with(hwnd, |l| if l.text_color == CLR_DEFAULT { unsafe { crate::GetSysColor(8) as i64 } } else { l.text_color as i64 }).unwrap_or(0),
        LVM_SETTEXTCOLOR => {
            with(hwnd, |l| l.text_color = lp as u32);
            invalidate(hwnd);
            1
        }
        LVM_GETTEXTBKCOLOR => with(hwnd, |l| if l.text_bk == CLR_DEFAULT { unsafe { crate::GetSysColor(5) as i64 } } else { l.text_bk as i64 }).unwrap_or(0),
        LVM_SETTEXTBKCOLOR => {
            with(hwnd, |l| l.text_bk = lp as u32);
            invalidate(hwnd);
            1
        }
        LVM_GETTOPINDEX => {
            let v = view(hwnd);
            if v != LVS_REPORT && v != LVS_LIST {
                return 0;
            }
            let h = row_height(hwnd);
            if v == LVS_REPORT {
                with(hwnd, |l| (l.sy / h) as i64).unwrap_or(0)
            } else {
                0
            }
        }
        LVM_GETCOUNTPERPAGE => {
            let c = client(hwnd);
            let h = row_height(hwnd).max(1);
            let rows = (c[3] - header_height(hwnd)) / h;
            match view(hwnd) {
                LVS_REPORT => rows as i64,
                LVS_LIST => (rows * (c[2] / cell_width(hwnd).max(1))) as i64,
                _ => count(hwnd) as i64,
            }
        }
        LVM_GETORIGIN => {
            if lp == 0 {
                return 0;
            }
            let v = view(hwnd);
            if v == LVS_REPORT || v == LVS_LIST {
                return 0;
            }
            let (sx, sy) = with(hwnd, |l| (l.sx, l.sy)).unwrap_or((0, 0));
            unsafe { *(lp as *mut [i32; 2]) = [-sx, -sy] };
            1
        }
        LVM_SETITEMSTATE => {
            if lp == 0 {
                return 0;
            }
            let lv = read_lvitem(lp as *const u8);
            set_state(hwnd, wp as i32, lv.state, lv.state_mask) as i64
        }
        LVM_GETITEMSTATE => (get_state(hwnd, wp as i32) & lp as u32) as i64,
        LVM_GETITEMTEXTW | LVM_GETITEMTEXTA => {
            if lp == 0 {
                return 0;
            }
            let lv = read_lvitem(lp as *const u8);
            let t = item_text(hwnd, wp as i32, lv.sub);
            crate::put_text(lv.text, lv.cap, &t, msg == LVM_GETITEMTEXTW) as i64
        }
        LVM_SETITEMTEXTW | LVM_SETITEMTEXTA => {
            if lp == 0 {
                return 0;
            }
            let mut lv = read_lvitem(lp as *const u8);
            lv.item = wp as i32;
            lv.mask = LVIF_TEXT;
            set_item(hwnd, &lv, msg == LVM_SETITEMTEXTW) as i64
        }
        LVM_SETITEMCOUNT => {
            let n = wp as i32;
            if owner_data(hwnd) {
                with(hwnd, |l| {
                    l.virtual_count = n.max(0);
                    l.virtual_sel.retain(|&k| k < n);
                    if l.focus >= n {
                        l.focus = -1;
                    }
                });
                layout(hwnd);
                invalidate(hwnd);
            }
            1
        }
        LVM_SORTITEMS => sort(hwnd, lp as usize, wp as i64, false) as i64,
        LVM_SORTITEMSEX => sort(hwnd, lp as usize, wp as i64, true) as i64,
        LVM_GETSELECTEDCOUNT => selected(hwnd).len() as i64,
        LVM_GETITEMSPACING => {
            let (w, h) = if wp != 0 {
                (cell_width(hwnd), row_height(hwnd))
            } else {
                with(hwnd, |l| l.icon_spacing).unwrap_or((75, 75))
            };
            ((h as i64) << 16) | (w as i64 & 0xFFFF)
        }
        LVM_SETICONSPACING => {
            let (w, h) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let old = with(hwnd, |l| {
                let old = l.icon_spacing;
                l.icon_spacing = (if w <= 0 { 75 } else { w }, if h <= 0 { 75 } else { h });
                old
            })
            .unwrap_or((75, 75));
            invalidate(hwnd);
            ((old.1 as i64) << 16) | (old.0 as i64 & 0xFFFF)
        }
        LVM_SETEXTENDEDLISTVIEWSTYLE => {
            let mask = if wp == 0 { u32::MAX } else { wp as u32 };
            let old = with(hwnd, |l| {
                let old = l.ex;
                l.ex = (l.ex & !mask) | (lp as u32 & mask);
                old
            })
            .unwrap_or(0);
            layout(hwnd);
            invalidate(hwnd);
            old as i64
        }
        LVM_GETEXTENDEDLISTVIEWSTYLE => with(hwnd, |l| l.ex as i64).unwrap_or(0),
        LVM_SETCOLUMNORDERARRAY => {
            if lp == 0 {
                return 0;
            }
            with(hwnd, |l| {
                for k in 0..(wp as usize).min(l.columns.len()) {
                    let i = unsafe { *(lp as *const i32).add(k) } as usize;
                    if let Some(c) = l.columns.get_mut(i) {
                        c.order = k as i32;
                    }
                }
            });
            let h = with(hwnd, |l| l.header).unwrap_or(0);
            unsafe { crate::SendMessageW(h, 0x1212, wp, lp) };
            invalidate(hwnd);
            1
        }
        LVM_GETCOLUMNORDERARRAY => {
            if lp == 0 {
                return 0;
            }
            let lay = column_layout(hwnd);
            if (wp as usize) < lay.len() {
                return 0;
            }
            for (k, c) in lay.iter().enumerate() {
                unsafe { *(lp as *mut i32).add(k) = c.0 as i32 };
            }
            1
        }
        LVM_GETSELECTIONMARK => with(hwnd, |l| l.mark as i64).unwrap_or(-1),
        LVM_SETSELECTIONMARK => with(hwnd, |l| core::mem::replace(&mut l.mark, lp as i32) as i64).unwrap_or(-1),
        LVM_SETVIEW => {
            let new = wp as u32;
            if new > 3 {
                return -1;
            }
            let st = style(hwnd);
            unsafe { crate::SetWindowLongPtrW(hwnd, -16, ((st & !3) | new) as i64) };
            if new == LVS_REPORT {
                ensure_header(hwnd);
            }
            with(hwnd, |l| {
                l.sx = 0;
                l.sy = 0;
            });
            layout(hwnd);
            invalidate(hwnd);
            1
        }
        LVM_GETVIEW => view(hwnd) as i64,
        LVM_ISITEMVISIBLE => {
            let i = wp as i32;
            if i < 0 || i >= count(hwnd) {
                return 0;
            }
            let r = item_rect(hwnd, i);
            let c = client(hwnd);
            (r[3] > header_height(hwnd) && r[1] < c[3] && r[2] > 0 && r[0] < c[2]) as i64
        }
        _ => def(),
    }
}

fn delete_all(hwnd: Hwnd) {
    if owner_data(hwnd) {
        with(hwnd, |l| {
            l.virtual_count = 0;
            l.virtual_sel.clear();
            l.focus = -1;
            l.mark = -1;
        });
    } else {
        let skip = nmlistview(hwnd, LVN_DELETEALLITEMS, -1, 0, 0, 0, 0, (0, 0), 0) != 0;
        if !skip {
            // Last to first.
            let n = count(hwnd);
            for i in (0..n).rev() {
                let param = item_param(hwnd, i);
                nmlistview(hwnd, LVN_DELETEITEM, i, 0, 0, 0, 0, (0, 0), param);
            }
        }
        with(hwnd, |l| {
            l.items.clear();
            l.focus = -1;
            l.mark = -1;
            l.sx = 0;
            l.sy = 0;
        });
    }
    layout(hwnd);
    invalidate(hwnd);
}
