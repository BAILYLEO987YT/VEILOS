//! The month calendar ("SysMonthCal32") and the date and time picker
//! ("SysDateTimePick32"), which drops one down. One month at a time, one day
//! selected.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::dllrt::Lock;
use crate::Hwnd;

const MCM_GETCURSEL: u32 = 0x1001;
const MCM_SETCURSEL: u32 = 0x1002;
const MCM_GETMAXSELCOUNT: u32 = 0x1003;
const MCM_GETMINREQRECT: u32 = 0x1009;
const MCM_SETTODAY: u32 = 0x100C;
const MCM_GETTODAY: u32 = 0x100D;
const MCM_GETFIRSTDAYOFWEEK: u32 = 0x1010;
const MCM_GETRANGE: u32 = 0x1011;
const MCM_SETRANGE: u32 = 0x1012;

const MCN_SELCHANGE: i32 = -749;
const MCN_SELECT: i32 = -746;

const DTM_GETSYSTEMTIME: u32 = 0x1001;
const DTM_SETSYSTEMTIME: u32 = 0x1002;
const DTM_GETRANGE: u32 = 0x1003;
const DTM_SETRANGE: u32 = 0x1004;
const DTM_SETFORMATA: u32 = 0x1005;
const DTM_GETMONTHCAL: u32 = 0x1008;
const DTM_SETFORMATW: u32 = 0x1032;

const DTS_UPDOWN: u32 = 0x1;
const DTS_SHOWNONE: u32 = 0x2;
const DTS_LONGDATEFORMAT: u32 = 0x4;
const DTS_TIMEFORMAT: u32 = 0x9;

const DTN_DATETIMECHANGE: i32 = -759;
const DTN_DROPDOWN: i32 = -754;
const DTN_CLOSEUP: i32 = -753;
const GDT_VALID: i32 = 0;
const GDT_NONE: i32 = 1;
const GDT_ERROR: i32 = -1;
const GDTR_MIN: u32 = 1;
const GDTR_MAX: u32 = 2;

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetLocalTime(st: *mut Date);
}

/// SYSTEMTIME.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Date {
    year: u16,
    month: u16,
    dow: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    ms: u16,
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in(y: i32, m: i32) -> i32 {
    match m {
        2 => 28 + leap(y) as i32,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Sunday is 0 (Sakamoto's method).
fn day_of_week(y: i32, m: i32, d: i32) -> i32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d).rem_euclid(7)
}

impl Date {
    fn valid(&self) -> bool {
        self.year >= 1601 && (1..=12).contains(&self.month) && self.day >= 1 && self.day as i32 <= days_in(self.year as i32, self.month as i32) && self.hour < 24 && self.minute < 60 && self.second < 60
    }
    fn with_dow(mut self) -> Date {
        self.dow = day_of_week(self.year as i32, self.month as i32, self.day as i32) as u16;
        self
    }
    fn key(&self) -> (u16, u16, u16) {
        (self.year, self.month, self.day)
    }
    /// A month away, keeping the day in range.
    fn add_months(mut self, n: i32) -> Date {
        let total = self.year as i32 * 12 + self.month as i32 - 1 + n;
        self.year = (total / 12) as u16;
        self.month = (total % 12 + 1) as u16;
        self.day = self.day.min(days_in(self.year as i32, self.month as i32) as u16);
        self.with_dow()
    }
    fn add_days(mut self, n: i32) -> Date {
        let mut d = self.day as i32 + n;
        while d < 1 {
            self = self.add_months(-1);
            d += days_in(self.year as i32, self.month as i32);
        }
        while d > days_in(self.year as i32, self.month as i32) {
            d -= days_in(self.year as i32, self.month as i32);
            self.day = 1;
            self = self.add_months(1);
        }
        self.day = d as u16;
        self.with_dow()
    }
}

fn today() -> Date {
    let mut d = Date::default();
    unsafe { GetLocalTime(&mut d) };
    d
}

fn clamp(d: Date, range: (Option<Date>, Option<Date>)) -> Date {
    match range {
        (Some(lo), _) if d.key() < lo.key() => lo,
        (_, Some(hi)) if d.key() > hi.key() => hi,
        _ => d,
    }
}

fn read_range(wp: u64, lp: i64) -> (Option<Date>, Option<Date>) {
    if lp == 0 {
        return (None, None);
    }
    let p = lp as *const Date;
    unsafe { ((wp as u32 & GDTR_MIN != 0).then(|| *p), (wp as u32 & GDTR_MAX != 0).then(|| *p.add(1))) }
}

fn write_range(lp: i64, range: (Option<Date>, Option<Date>)) -> i64 {
    if lp == 0 {
        return 0;
    }
    let p = lp as *mut Date;
    unsafe {
        *p = range.0.unwrap_or_default();
        *p.add(1) = range.1.unwrap_or_default();
    }
    (if range.0.is_some() { GDTR_MIN } else { 0 } | if range.1.is_some() { GDTR_MAX } else { 0 }) as i64
}

// -----| the month calendar |-----

struct Cal {
    sel: Date,
    today: Date,
    /// The month on show (its first day).
    shown: Date,
    range: (Option<Date>, Option<Date>),
    font: u64,
}

static CALS: Lock<BTreeMap<Hwnd, Cal>> = Lock::new(BTreeMap::new());

fn cal<R>(hwnd: Hwnd, f: impl FnOnce(&mut Cal) -> R) -> Option<R> {
    CALS.lock().get_mut(&hwnd).map(f)
}

/// The least width of a cell, its height, and where the day grid starts.
fn cal_least(hwnd: Hwnd) -> (i32, i32, i32) {
    let h = crate::font_height(hwnd, cal(hwnd, |c| c.font).unwrap_or(0));
    (h * 3 / 2 + 3, h + 4, (h + 6) + (h + 4))
}

/// The same, with the columns sharing the window's width.
fn cal_metrics(hwnd: Hwnd) -> (i32, i32, i32) {
    let (cw, ch, top) = cal_least(hwnd);
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    (cw.max(c[2] / 7), ch, top)
}

fn cal_paint(hwnd: Hwnd, hdc: u64) {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let Some((sel, now, shown, font)) = cal(hwnd, |x| (x.sel, x.today, x.shown, x.font)) else { return };
    unsafe { crate::FillRect(hdc, &c, crate::GetSysColorBrush(5)) };
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe {
        crate::SetBkMode(hdc, 1);
        crate::SetTextColor(hdc, crate::GetSysColor(8));
    }
    let (cw, ch, top) = cal_metrics(hwnd);
    let centre = |r: [i32; 4], s: &str, hdc: u64| {
        let mut t: Vec<u16> = s.encode_utf16().collect();
        let mut rr = r;
        // DT_SINGLELINE | DT_VCENTER | DT_CENTER
        unsafe { crate::DrawTextW(hdc, t.as_mut_ptr(), t.len() as i32, &mut rr, 0x20 | 0x4 | 0x1) };
    };
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let title = alloc::format!("{} {}", MONTHS[shown.month as usize - 1], shown.year);
    let head = top - ch;
    centre([cw, 0, c[2] - cw, head], &title, hdc);
    centre([0, 0, cw, head], "<", hdc);
    centre([c[2] - cw, 0, c[2], head], ">", hdc);
    for (i, d) in ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"].iter().enumerate() {
        centre([i as i32 * cw, head, (i as i32 + 1) * cw, top], d, hdc);
    }
    unsafe { crate::FillRect(hdc, &[0, top - 1, 7 * cw, top], crate::GetSysColorBrush(16)) };
    let first = day_of_week(shown.year as i32, shown.month as i32, 1);
    for day in 1..=days_in(shown.year as i32, shown.month as i32) {
        let k = first + day - 1;
        let (col, row) = (k % 7, k / 7);
        let r = [col * cw, top + row * ch, (col + 1) * cw, top + (row + 1) * ch];
        let is = |d: Date| d.year == shown.year && d.month == shown.month && d.day as i32 == day;
        if is(sel) {
            unsafe {
                crate::FillRect(hdc, &r, crate::GetSysColorBrush(13));
                crate::SetTextColor(hdc, crate::GetSysColor(14));
            }
        } else {
            unsafe { crate::SetTextColor(hdc, crate::GetSysColor(8)) };
        }
        centre(r, &alloc::format!("{}", day), hdc);
        if is(now) {
            unsafe { crate::DrawFocusRect(hdc, &r) };
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

/// NMSELCHANGE: stSelStart, stSelEnd after the NMHDR.
fn cal_notify(hwnd: Hwnd, code: i32) {
    let Some(d) = cal(hwnd, |x| x.sel) else { return };
    let mut e = [0u8; 32];
    unsafe {
        core::ptr::copy_nonoverlapping(&d as *const Date as *const u8, e.as_mut_ptr(), 16);
        core::ptr::copy_nonoverlapping(&d as *const Date as *const u8, e.as_mut_ptr().add(16), 16);
    }
    crate::notify(hwnd, code, &e);
}

fn cal_set(hwnd: Hwnd, d: Date, code: Option<i32>) {
    cal(hwnd, |x| {
        x.sel = clamp(d, x.range).with_dow();
        x.shown = x.sel;
        x.shown.day = 1;
    });
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
    if let Some(c) = code {
        cal_notify(hwnd, c);
    }
}

unsafe extern "system" fn cal_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0081 => {
            let t = today();
            let mut shown = t;
            shown.day = 1;
            CALS.lock().insert(hwnd, Cal { sel: t, today: t, shown, range: (None, None), font: 0 });
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0082 => {
            CALS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            cal_paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0030 => {
            cal(hwnd, |x| x.font = wp);
            0
        }
        0x0087 => 0x1,
        0x0100 => {
            let Some(s) = cal(hwnd, |x| x.sel) else { return 0 };
            let to = match wp {
                0x25 => s.add_days(-1),
                0x27 => s.add_days(1),
                0x26 => s.add_days(-7),
                0x28 => s.add_days(7),
                0x21 => s.add_months(-1),
                0x22 => s.add_months(1),
                0x0D => {
                    cal_notify(hwnd, MCN_SELECT);
                    return 0;
                }
                _ => return 0,
            };
            cal_set(hwnd, to, Some(MCN_SELCHANGE));
            0
        }
        0x0201 => {
            unsafe { crate::SetFocus(hwnd) };
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let (cw, ch, top) = cal_metrics(hwnd);
            let Some(shown) = cal(hwnd, |c| c.shown) else { return 0 };
            let mut c = [0i32; 4];
            unsafe { crate::GetClientRect(hwnd, &mut c) };
            if y < top - ch {
                let step = if x < cw { -1 } else if x >= c[2] - cw { 1 } else { 0 };
                if step != 0 {
                    cal(hwnd, |s| s.shown = s.shown.add_months(step));
                    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
                }
                return 0;
            }
            if y < top {
                return 0;
            }
            let first = day_of_week(shown.year as i32, shown.month as i32, 1);
            let day = (y - top) / ch * 7 + x / cw - first + 1;
            if day >= 1 && day <= days_in(shown.year as i32, shown.month as i32) && x / cw < 7 {
                let mut d = cal(hwnd, |s| s.sel).unwrap_or(shown);
                d.year = shown.year;
                d.month = shown.month;
                d.day = day as u16;
                cal_set(hwnd, d, Some(MCN_SELCHANGE));
                cal_notify(hwnd, MCN_SELECT);
            }
            0
        }
        MCM_GETCURSEL => {
            if lp == 0 {
                return 0;
            }
            let Some(mut d) = cal(hwnd, |x| x.sel) else { return 0 };
            // The date only.
            d.hour = 0;
            d.minute = 0;
            d.second = 0;
            d.ms = 0;
            unsafe { *(lp as *mut Date) = d };
            1
        }
        MCM_SETCURSEL => {
            if lp == 0 {
                return 0;
            }
            let d = unsafe { *(lp as *const Date) };
            if !d.valid() {
                return 0;
            }
            cal_set(hwnd, d, None);
            1
        }
        MCM_GETMAXSELCOUNT => 1,
        MCM_GETMINREQRECT => {
            if lp == 0 {
                return 0;
            }
            let (cw, ch, top) = cal_least(hwnd);
            unsafe { *(lp as *mut [i32; 4]) = [0, 0, 7 * cw, top + 6 * ch] };
            1
        }
        MCM_SETTODAY => {
            if lp != 0 {
                let d = unsafe { *(lp as *const Date) };
                cal(hwnd, |x| x.today = d);
            }
            0
        }
        MCM_GETTODAY => {
            if lp == 0 {
                return 0;
            }
            let Some(d) = cal(hwnd, |x| x.today) else { return 0 };
            unsafe { *(lp as *mut Date) = d };
            1
        }
        MCM_GETFIRSTDAYOFWEEK => 6,
        MCM_GETRANGE => write_range(lp, cal(hwnd, |x| x.range).unwrap_or((None, None))),
        MCM_SETRANGE => {
            let r = read_range(wp, lp);
            cal(hwnd, |x| x.range = r);
            1
        }
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

// -----| the picker |-----

struct Picker {
    value: Date,
    none: bool,
    range: (Option<Date>, Option<Date>),
    format: Vec<u16>,
    font: u64,
    field: usize,
    popup: Hwnd,
    has_focus: bool,
}

static PICKERS: Lock<BTreeMap<Hwnd, Picker>> = Lock::new(BTreeMap::new());

fn pick<R>(hwnd: Hwnd, f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKERS.lock().get_mut(&hwnd).map(f)
}

fn style(hwnd: Hwnd) -> u32 {
    unsafe { crate::GetWindowLongPtrW(hwnd, -16) as u32 }
}

/// The format pieces: literal text, or a field (and which).
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

/// The value as text, and each field's span in it.
fn render(hwnd: Hwnd) -> (Vec<u16>, Vec<(Field, usize, usize)>) {
    let Some((v, fmt)) = pick(hwnd, |p| (p.value, p.format.clone())) else { return (Vec::new(), Vec::new()) };
    let st = style(hwnd);
    let default: &str = if st & DTS_TIMEFORMAT == DTS_TIMEFORMAT {
        "h:mm:ss tt"
    } else if st & DTS_LONGDATEFORMAT != 0 {
        "dddd, MMMM dd, yyyy"
    } else {
        "M/d/yyyy"
    };
    let f: Vec<u16> = if fmt.is_empty() { default.encode_utf16().collect() } else { fmt };
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    let mut out: Vec<u16> = Vec::new();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < f.len() {
        let c = f[i];
        if c == b'\'' as u16 {
            i += 1;
            while i < f.len() && f[i] != b'\'' as u16 {
                out.push(f[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut run = 1;
        while i + run < f.len() && f[i + run] == c {
            run += 1;
        }
        let dow = day_of_week(v.year as i32, v.month as i32, v.day as i32) as usize;
        let h12 = if v.hour % 12 == 0 { 12 } else { v.hour % 12 };
        let (text, field) = match (c as u8 as char, run) {
            ('d', 1) => (alloc::format!("{}", v.day), Some(Field::Day)),
            ('d', 2) => (alloc::format!("{:02}", v.day), Some(Field::Day)),
            ('d', 3) => (alloc::string::String::from(&DAYS[dow][..3]), Some(Field::Day)),
            ('d', _) => (alloc::string::String::from(DAYS[dow]), Some(Field::Day)),
            ('M', 1) => (alloc::format!("{}", v.month), Some(Field::Month)),
            ('M', 2) => (alloc::format!("{:02}", v.month), Some(Field::Month)),
            ('M', 3) => (alloc::string::String::from(&MONTHS[v.month as usize - 1][..3]), Some(Field::Month)),
            ('M', _) => (alloc::string::String::from(MONTHS[v.month as usize - 1]), Some(Field::Month)),
            ('y', 1 | 2) => (alloc::format!("{:02}", v.year % 100), Some(Field::Year)),
            ('y', _) => (alloc::format!("{}", v.year), Some(Field::Year)),
            ('h', 1) => (alloc::format!("{}", h12), Some(Field::Hour)),
            ('h', _) => (alloc::format!("{:02}", h12), Some(Field::Hour)),
            ('H', 1) => (alloc::format!("{}", v.hour), Some(Field::Hour)),
            ('H', _) => (alloc::format!("{:02}", v.hour), Some(Field::Hour)),
            ('m', 1) => (alloc::format!("{}", v.minute), Some(Field::Minute)),
            ('m', _) => (alloc::format!("{:02}", v.minute), Some(Field::Minute)),
            ('s', 1) => (alloc::format!("{}", v.second), Some(Field::Second)),
            ('s', _) => (alloc::format!("{:02}", v.second), Some(Field::Second)),
            ('t', _) => (alloc::string::String::from(if v.hour < 12 { "AM" } else { "PM" }), Some(Field::Hour)),
            _ => {
                let lit: alloc::string::String = core::iter::repeat(char::from_u32(c as u32).unwrap_or('?')).take(run).collect();
                (lit, None)
            }
        };
        let at = out.len();
        out.extend(text.encode_utf16());
        if let Some(fl) = field {
            spans.push((fl, at, out.len()));
        }
        i += run;
    }
    (out, spans)
}

fn picker_paint(hwnd: Hwnd, hdc: u64) {
    let mut c = [0i32; 4];
    unsafe { crate::GetClientRect(hwnd, &mut c) };
    let Some((font, field, focus, none)) = pick(hwnd, |p| (p.font, p.field, p.has_focus, p.none)) else { return };
    unsafe { crate::FillRect(hdc, &c, crate::GetSysColorBrush(5)) };
    let edge = unsafe { crate::GetSysColorBrush(16) };
    unsafe {
        crate::FillRect(hdc, &[0, 0, c[2], 1], edge);
        crate::FillRect(hdc, &[0, c[3] - 1, c[2], c[3]], edge);
        crate::FillRect(hdc, &[0, 0, 1, c[3]], edge);
        crate::FillRect(hdc, &[c[2] - 1, 0, c[2], c[3]], edge);
    }
    let old = unsafe { crate::SelectObject(hdc, crate::font_or_default(font)) };
    unsafe { crate::SetBkMode(hdc, 1) };
    let (text, spans) = render(hwnd);
    let color = unsafe { crate::GetSysColor(if none { 17 } else { 8 }) };
    let y = (c[3] - crate::text_size(hdc, &text).1) / 2;
    let mut x = 4;
    let piece = |hdc: u64, t: &[u16], x: &mut i32, color: u32| {
        let mut v = t.to_vec();
        let w = crate::text_size(hdc, t).0;
        let mut r = [*x, y, *x + w + 2, c[3]];
        unsafe {
            crate::SetTextColor(hdc, color);
            crate::DrawTextW(hdc, v.as_mut_ptr(), v.len() as i32, &mut r, 0x800 | 0x20 | 0x100);
        }
        *x += w;
    };
    let mut at = 0;
    for (k, s) in spans.iter().enumerate() {
        piece(hdc, &text[at..s.1], &mut x, color);
        if focus && k == field {
            let w = crate::text_size(hdc, &text[s.1..s.2]).0;
            unsafe { crate::FillRect(hdc, &[x, y, x + w, y + crate::text_size(hdc, &text).1], crate::GetSysColorBrush(13)) };
            piece(hdc, &text[s.1..s.2], &mut x, unsafe { crate::GetSysColor(14) });
        } else {
            piece(hdc, &text[s.1..s.2], &mut x, color);
        }
        at = s.2;
    }
    piece(hdc, &text[at..], &mut x, color);
    // The arrow that drops the calendar down (or the up-down's).
    let st = style(hwnd);
    let bw = c[3].min(18);
    let b = [c[2] - bw - 1, 1, c[2] - 1, c[3] - 1];
    unsafe { crate::FillRect(hdc, &b, crate::GetSysColorBrush(15)) };
    let (mx, my) = ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2);
    let ink = unsafe { crate::GetSysColorBrush(18) };
    if st & DTS_UPDOWN != 0 || st & DTS_TIMEFORMAT == DTS_TIMEFORMAT {
        for k in 0..3 {
            unsafe {
                crate::FillRect(hdc, &[mx - k, my - 4 + k, mx + k + 1, my - 3 + k], ink);
                crate::FillRect(hdc, &[mx - k, my + 3 - k, mx + k + 1, my + 4 - k], ink);
            }
        }
    } else {
        for k in 0..4 {
            unsafe { crate::FillRect(hdc, &[mx - 3 + k, my - 1 + k, mx + 4 - k, my + k], ink) };
        }
    }
    unsafe { crate::SelectObject(hdc, old) };
}

/// NMDATETIMECHANGE: flags, dwFlags then the time.
fn picker_changed(hwnd: Hwnd) {
    let Some((v, none)) = pick(hwnd, |p| (p.value, p.none)) else { return };
    let mut e = [0u8; 24];
    e[0..4].copy_from_slice(&(if none { GDT_NONE } else { GDT_VALID }).to_le_bytes());
    unsafe { core::ptr::copy_nonoverlapping(&v as *const Date as *const u8, e.as_mut_ptr().add(4), 16) };
    crate::notify(hwnd, DTN_DATETIMECHANGE, &e);
    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
}

fn step_field(hwnd: Hwnd, by: i32) {
    let (_, spans) = render(hwnd);
    let Some((v, field, range)) = pick(hwnd, |p| (p.value, p.field, p.range)) else { return };
    let Some(&(f, _, _)) = spans.get(field) else { return };
    let mut n = v;
    match f {
        Field::Year => n = n.add_months(by * 12),
        Field::Month => n = n.add_months(by),
        Field::Day => n = n.add_days(by),
        Field::Hour => n.hour = (n.hour as i32 + by).rem_euclid(24) as u16,
        Field::Minute => n.minute = (n.minute as i32 + by).rem_euclid(60) as u16,
        Field::Second => n.second = (n.second as i32 + by).rem_euclid(60) as u16,
    }
    let n = clamp(n, range).with_dow();
    pick(hwnd, |p| {
        p.value = n;
        p.none = false;
    });
    picker_changed(hwnd);
}

fn close_popup(hwnd: Hwnd) {
    let cal_win = pick(hwnd, |p| core::mem::replace(&mut p.popup, 0)).unwrap_or(0);
    if cal_win != 0 {
        unsafe { crate::DestroyWindow(cal_win) };
        crate::notify(hwnd, DTN_CLOSEUP, &[]);
        unsafe { crate::SetFocus(hwnd) };
    }
}

fn open_popup(hwnd: Hwnd) {
    if pick(hwnd, |p| p.popup).unwrap_or(0) != 0 {
        close_popup(hwnd);
        return;
    }
    crate::notify(hwnd, DTN_DROPDOWN, &[]);
    let class = crate::wide("SysMonthCal32");
    let mut r = [0i32; 4];
    unsafe { crate::GetWindowRect(hwnd, &mut r) };
    // WS_POPUP | WS_VISIBLE | WS_BORDER, owned by the picker's window.
    let owner = unsafe { crate::GetParent(hwnd) };
    let c = unsafe { crate::CreateWindowExW(0, class.as_ptr(), core::ptr::null(), 0x9080_0000, r[0], r[3], 10, 10, owner, 0, 0, 0) };
    if c == 0 {
        return;
    }
    let mut size = [0i32; 4];
    unsafe { crate::SendMessageW(c, MCM_GETMINREQRECT, 0, size.as_mut_ptr() as i64) };
    unsafe { crate::SetWindowPos(c, 0, 0, 0, size[2] + 2, size[3] + 2, 0x2 | 0x4 | 0x10) };
    let Some(v) = pick(hwnd, |p| {
        p.popup = c;
        p.value
    }) else {
        return;
    };
    unsafe {
        crate::SendMessageW(c, MCM_SETCURSEL, 0, &v as *const Date as i64);
        crate::SetWindowLongPtrW(c, -21, hwnd as i64); // GWLP_USERDATA: its picker
        crate::SetFocus(c);
    }
    POPUP_OWNERS.lock().insert(c, hwnd);
    crate::SetWindowSubclass(c, popup_subclass as *const () as usize, 1, hwnd);
}

static POPUP_OWNERS: Lock<BTreeMap<Hwnd, Hwnd>> = Lock::new(BTreeMap::new());

/// The dropped-down calendar: a pick (or leaving) closes it.
unsafe extern "system" fn popup_subclass(c: Hwnd, msg: u32, wp: u64, lp: i64, _id: u64, picker: u64) -> i64 {
    let r = crate::DefSubclassProc(c, msg, wp, lp);
    match msg {
        // A day was clicked, or Return: the calendar's selection is the value.
        0x0201 | 0x0100 if msg == 0x0201 || wp == 0x0D || wp == 0x1B => {
            let mut d = Date::default();
            unsafe { crate::SendMessageW(c, MCM_GETCURSEL, 0, &mut d as *mut Date as i64) };
            let was = pick(picker, |p| p.value).unwrap_or_default();
            let mine = (d.year, d.month, d.day) != (was.year, was.month, was.day);
            let mut grid = [0i32; 4];
            let (_, _, top) = cal_metrics(c);
            unsafe { crate::GetClientRect(c, &mut grid) };
            let on_day = msg != 0x0201 || ((lp >> 16) & 0xFFFF) as i16 as i32 >= top;
            if on_day && wp != 0x1B {
                if mine {
                    pick(picker, |p| {
                        p.value.year = d.year;
                        p.value.month = d.month;
                        p.value.day = d.day;
                        p.value = p.value.with_dow();
                        p.none = false;
                    });
                    picker_changed(picker);
                }
                unsafe { crate::PostMessageW(picker, CLOSE_POPUP, 0, 0) };
            } else if msg == 0x0100 {
                unsafe { crate::PostMessageW(picker, CLOSE_POPUP, 0, 0) };
            }
        }
        0x0082 => {
            POPUP_OWNERS.lock().remove(&c);
        }
        _ => {}
    }
    r
}

const CLOSE_POPUP: u32 = 0x7F20;

unsafe extern "system" fn picker_proc(hwnd: Hwnd, msg: u32, wp: u64, lp: i64) -> i64 {
    match msg {
        0x0081 => {
            PICKERS.lock().insert(hwnd, Picker { value: today().with_dow(), none: false, range: (None, None), format: Vec::new(), font: 0, field: 0, popup: 0, has_focus: false });
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x0082 => {
            PICKERS.lock().remove(&hwnd);
            unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) }
        }
        0x000F => {
            let mut ps = [0u8; 72];
            let hdc = unsafe { crate::BeginPaint(hwnd, ps.as_mut_ptr()) };
            picker_paint(hwnd, hdc);
            unsafe { crate::EndPaint(hwnd, ps.as_ptr()) };
            0
        }
        0x0014 => 1,
        0x0030 => {
            pick(hwnd, |p| p.font = wp);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            0
        }
        0x0031 => pick(hwnd, |p| p.font as i64).unwrap_or(0),
        0x0007 | 0x0008 => {
            pick(hwnd, |p| p.has_focus = msg == 0x0007);
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            0
        }
        0x0087 => 0x1 | 0x4,
        // The value as text.
        0x000D => {
            let (t, _) = render(hwnd);
            crate::put_text(lp as u64, wp as i32, &t, true) as i64
        }
        0x000E => render(hwnd).0.len() as i64,
        0x0100 => {
            let n = render(hwnd).1.len();
            match wp {
                0x26 | 0x6B => step_field(hwnd, 1),
                0x28 | 0x6D => step_field(hwnd, -1),
                0x25 => {
                    pick(hwnd, |p| p.field = p.field.saturating_sub(1));
                    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
                }
                0x27 => {
                    pick(hwnd, |p| p.field = (p.field + 1).min(n.saturating_sub(1)));
                    unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
                }
                0x73 => open_popup(hwnd), // F4
                _ => {}
            }
            0
        }
        0x0201 => {
            unsafe { crate::SetFocus(hwnd) };
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
            let mut c = [0i32; 4];
            unsafe { crate::GetClientRect(hwnd, &mut c) };
            let bw = c[3].min(18);
            if x >= c[2] - bw - 1 {
                let st = style(hwnd);
                if st & DTS_UPDOWN != 0 || st & DTS_TIMEFORMAT == DTS_TIMEFORMAT {
                    step_field(hwnd, if y < c[3] / 2 { 1 } else { -1 });
                } else {
                    open_popup(hwnd);
                }
                return 0;
            }
            // The field clicked.
            let (text, spans) = render(hwnd);
            let font = pick(hwnd, |p| p.font).unwrap_or(0);
            let dc = unsafe { crate::GetDC(hwnd) };
            let old = unsafe { crate::SelectObject(dc, crate::font_or_default(font)) };
            for (k, s) in spans.iter().enumerate() {
                if x - 4 < crate::text_size(dc, &text[..s.2]).0 {
                    pick(hwnd, |p| p.field = k);
                    break;
                }
            }
            unsafe {
                crate::SelectObject(dc, old);
                crate::ReleaseDC(hwnd, dc);
                crate::InvalidateRect(hwnd, core::ptr::null(), 1);
            }
            0
        }
        CLOSE_POPUP => {
            close_popup(hwnd);
            0
        }
        DTM_GETSYSTEMTIME => {
            if lp == 0 {
                return GDT_ERROR as i64;
            }
            let Some((v, none)) = pick(hwnd, |p| (p.value, p.none)) else { return GDT_ERROR as i64 };
            if none {
                return GDT_NONE as i64;
            }
            unsafe { *(lp as *mut Date) = v };
            GDT_VALID as i64
        }
        DTM_SETSYSTEMTIME => {
            if wp as i32 == GDT_NONE {
                if style(hwnd) & DTS_SHOWNONE == 0 {
                    return 0;
                }
                pick(hwnd, |p| p.none = true);
                unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
                return 1;
            }
            if lp == 0 {
                return 0;
            }
            let d = unsafe { *(lp as *const Date) };
            if !d.valid() {
                return 0;
            }
            pick(hwnd, |p| {
                p.value = clamp(d, p.range).with_dow();
                p.none = false;
            });
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            1
        }
        DTM_GETRANGE => write_range(lp, pick(hwnd, |p| p.range).unwrap_or((None, None))),
        DTM_SETRANGE => {
            let r = read_range(wp, lp);
            pick(hwnd, |p| {
                p.range = r;
                p.value = clamp(p.value, r);
            });
            1
        }
        DTM_SETFORMATW | DTM_SETFORMATA => {
            let f = if lp == 0 { Vec::new() } else { crate::text_arg(lp as u64, msg == DTM_SETFORMATW) };
            pick(hwnd, |p| {
                p.format = f;
                p.field = 0;
            });
            unsafe { crate::InvalidateRect(hwnd, core::ptr::null(), 1) };
            1
        }
        DTM_GETMONTHCAL => pick(hwnd, |p| p.popup as i64).unwrap_or(0),
        _ => unsafe { crate::DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

pub fn register_classes() {
    crate::register("SysMonthCal32", cal_proc as *const () as usize, true);
    crate::register("SysDateTimePick32", picker_proc as *const () as usize, true);
}
