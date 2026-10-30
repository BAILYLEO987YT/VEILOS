//! Time: time_t, clock, struct tm, strftime. VEILOS keeps UTC, so local
//! time is UTC and there's no daylight saving.

use crate::{GetSystemTimeAsFileTime, GetTickCount64};
use core::fmt::Write;

/// FILETIME of 1970-01-01, in 100 ns units since 1601.
const UNIX_EPOCH: u64 = 116_444_736_000_000_000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
}

fn now() -> i64 {
    let mut ft = 0u64;
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    (ft.saturating_sub(UNIX_EPOCH) / 10_000_000) as i64
}

#[no_mangle]
pub unsafe extern "C" fn _time64(out: *mut i64) -> i64 {
    let t = now();
    if !out.is_null() {
        unsafe { *out = t };
    }
    t
}

#[no_mangle]
pub unsafe extern "C" fn time(out: *mut i64) -> i64 {
    unsafe { _time64(out) }
}

#[no_mangle]
pub unsafe extern "C" fn _time32(out: *mut i32) -> i32 {
    let t = now() as i32;
    if !out.is_null() {
        unsafe { *out = t };
    }
    t
}

static mut START_TICKS: u64 = 0;

pub fn init() {
    unsafe { START_TICKS = GetTickCount64() };
}

/// Milliseconds since the program started (CLOCKS_PER_SEC is 1000).
#[no_mangle]
pub extern "C" fn clock() -> i32 {
    (unsafe { GetTickCount64() - START_TICKS }) as i32
}

#[no_mangle]
pub extern "C" fn _difftime64(a: i64, b: i64) -> f64 {
    (a - b) as f64
}

#[no_mangle]
pub extern "C" fn difftime(a: i64, b: i64) -> f64 {
    (a - b) as f64
}

#[no_mangle]
pub extern "C" fn _difftime32(a: i32, b: i32) -> f64 {
    (a as i64 - b as i64) as f64
}

/// Days since 1970-01-01 to (year, month 1-12, day).
fn civil(days: i64) -> (i64, i32, i32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i32;
    let y = yoe + era * 400 + (m <= 2) as i64;
    (y, m, d)
}

/// (year, month 1-12, day) to days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn broken_down(t: i64) -> Option<Tm> {
    // The CRT's range: 1970 to 3000-12-31 23:59:59.
    if !(0..=32_535_215_999).contains(&t) {
        return None;
    }
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400) as i32;
    let (y, m, d) = civil(days);
    Some(Tm {
        sec: secs % 60,
        min: secs / 60 % 60,
        hour: secs / 3600,
        mday: d,
        mon: m - 1,
        year: (y - 1900) as i32,
        wday: ((days + 4).rem_euclid(7)) as i32,
        yday: (days - days_from_civil(y, 1, 1)) as i32,
        isdst: 0,
    })
}

static mut TM: Tm = Tm { sec: 0, min: 0, hour: 0, mday: 0, mon: 0, year: 0, wday: 0, yday: 0, isdst: 0 };

unsafe fn into_static(t: i64) -> *mut Tm {
    match broken_down(t) {
        Some(tm) => {
            unsafe { TM = tm };
            &raw mut TM
        }
        None => {
            crate::set_errno(crate::errno::EINVAL);
            core::ptr::null_mut()
        }
    }
}

unsafe fn into(out: *mut Tm, t: i64) -> i32 {
    if out.is_null() {
        crate::invalid_parameter();
        return crate::errno::EINVAL;
    }
    match broken_down(t) {
        Some(tm) => {
            unsafe { *out = tm };
            0
        }
        None => {
            unsafe { *out = Tm { mday: -1, mon: -1, year: -1, ..Tm::default() } };
            crate::errno::EINVAL
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn _gmtime64(t: *const i64) -> *mut Tm {
    unsafe { into_static(*t) }
}

#[no_mangle]
pub unsafe extern "C" fn _gmtime32(t: *const i32) -> *mut Tm {
    unsafe { into_static(*t as i64) }
}

#[no_mangle]
pub unsafe extern "C" fn gmtime(t: *const i64) -> *mut Tm {
    unsafe { into_static(*t) }
}

#[no_mangle]
pub unsafe extern "C" fn _localtime64(t: *const i64) -> *mut Tm {
    unsafe { into_static(*t) }
}

#[no_mangle]
pub unsafe extern "C" fn _localtime32(t: *const i32) -> *mut Tm {
    unsafe { into_static(*t as i64) }
}

#[no_mangle]
pub unsafe extern "C" fn localtime(t: *const i64) -> *mut Tm {
    unsafe { into_static(*t) }
}

#[no_mangle]
pub unsafe extern "C" fn _gmtime64_s(out: *mut Tm, t: *const i64) -> i32 {
    unsafe { into(out, *t) }
}

#[no_mangle]
pub unsafe extern "C" fn _gmtime32_s(out: *mut Tm, t: *const i32) -> i32 {
    unsafe { into(out, *t as i64) }
}

#[no_mangle]
pub unsafe extern "C" fn _localtime64_s(out: *mut Tm, t: *const i64) -> i32 {
    unsafe { into(out, *t) }
}

#[no_mangle]
pub unsafe extern "C" fn _localtime32_s(out: *mut Tm, t: *const i32) -> i32 {
    unsafe { into(out, *t as i64) }
}

/// struct tm (normalising out-of-range fields) to time_t.
unsafe fn make_time(tm: *mut Tm) -> i64 {
    let t = unsafe { &mut *tm };
    let mut year = t.year as i64 + 1900;
    let mut mon = t.mon as i64;
    year += mon.div_euclid(12);
    mon = mon.rem_euclid(12);
    let days = days_from_civil(year, mon + 1, 1) + t.mday as i64 - 1;
    let secs = days * 86_400 + t.hour as i64 * 3600 + t.min as i64 * 60 + t.sec as i64;
    match broken_down(secs) {
        Some(n) => {
            *t = n;
            secs
        }
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn _mktime64(tm: *mut Tm) -> i64 {
    unsafe { make_time(tm) }
}

#[no_mangle]
pub unsafe extern "C" fn mktime(tm: *mut Tm) -> i64 {
    unsafe { make_time(tm) }
}

#[no_mangle]
pub unsafe extern "C" fn _mktime32(tm: *mut Tm) -> i32 {
    unsafe { make_time(tm) as i32 }
}

#[no_mangle]
pub unsafe extern "C" fn _mkgmtime64(tm: *mut Tm) -> i64 {
    unsafe { make_time(tm) }
}

#[no_mangle]
pub unsafe extern "C" fn _mkgmtime(tm: *mut Tm) -> i64 {
    unsafe { make_time(tm) }
}

#[no_mangle]
pub unsafe extern "C" fn _mkgmtime32(tm: *mut Tm) -> i32 {
    unsafe { make_time(tm) as i32 }
}

// -----| time zones: UTC |-----

#[no_mangle]
pub static mut _timezone: i32 = 0;
#[no_mangle]
pub static mut _daylight: i32 = 0;
#[no_mangle]
pub static mut _dstbias: i32 = 0;
static mut UTC: [u8; 4] = *b"UTC\0";
#[no_mangle]
pub static mut _tzname: [*mut u8; 2] = [&raw mut UTC as *mut u8, &raw mut UTC as *mut u8];

#[no_mangle]
pub extern "C" fn _tzset() {}

#[no_mangle]
pub extern "C" fn __timezone() -> *mut i32 {
    &raw mut _timezone
}

#[no_mangle]
pub extern "C" fn __daylight() -> *mut i32 {
    &raw mut _daylight
}

#[no_mangle]
pub extern "C" fn __dstbias() -> *mut i32 {
    &raw mut _dstbias
}

#[no_mangle]
pub extern "C" fn __tzname() -> *mut [*mut u8; 2] {
    &raw mut _tzname
}

#[no_mangle]
pub unsafe extern "C" fn _get_timezone(v: *mut i32) -> i32 {
    unsafe { *v = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _get_daylight(v: *mut i32) -> i32 {
    unsafe { *v = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _get_dstbias(v: *mut i32) -> i32 {
    unsafe { *v = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _get_tzname(n: *mut usize, buf: *mut u8, size: usize, _index: i32) -> i32 {
    unsafe {
        *n = 4;
        if !buf.is_null() && size >= 4 {
            core::ptr::copy_nonoverlapping(b"UTC\0".as_ptr(), buf, 4);
        }
    }
    0
}

/// C11 timespec_get (TIME_UTC = 1).
#[repr(C)]
pub struct Timespec {
    sec: i64,
    nsec: i32,
}

#[no_mangle]
pub unsafe extern "C" fn _timespec64_get(ts: *mut Timespec, base: i32) -> i32 {
    if base != 1 {
        return 0;
    }
    let mut ft = 0u64;
    unsafe { GetSystemTimeAsFileTime(&mut ft) };
    let since = ft.saturating_sub(UNIX_EPOCH);
    unsafe {
        *ts = Timespec { sec: (since / 10_000_000) as i64, nsec: (since % 10_000_000 * 100) as i32 };
    }
    base
}

#[no_mangle]
pub unsafe extern "C" fn timespec_get(ts: *mut Timespec, base: i32) -> i32 {
    unsafe { _timespec64_get(ts, base) }
}

// -----| text |-----

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// A small fixed buffer that core::fmt can write into.
struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}

impl<const N: usize> Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.n + s.len() > N {
            return Err(core::fmt::Error);
        }
        self.b[self.n..self.n + s.len()].copy_from_slice(s.as_bytes());
        self.n += s.len();
        Ok(())
    }
}

fn name(list: &[&'static str], i: i32) -> &'static str {
    list.get(i as usize).copied().unwrap_or("?")
}

/// "Wed Jan 02 02:03:55 1980\n"
fn asc(tm: &Tm, out: &mut Buf<32>) {
    let _ = write!(
        out,
        "{} {} {:02} {:02}:{:02}:{:02} {}\n\0",
        &name(&DAYS, tm.wday)[..3],
        &name(&MONTHS, tm.mon)[..3],
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec,
        tm.year + 1900
    );
}

static mut ASC: [u8; 32] = [0; 32];

#[no_mangle]
pub unsafe extern "C" fn asctime(tm: *const Tm) -> *mut u8 {
    let mut b = Buf { b: [0; 32], n: 0 };
    asc(unsafe { &*tm }, &mut b);
    unsafe {
        ASC = b.b;
        &raw mut ASC as *mut u8
    }
}

#[no_mangle]
pub unsafe extern "C" fn asctime_s(buf: *mut u8, size: usize, tm: *const Tm) -> i32 {
    let mut b = Buf { b: [0; 32], n: 0 };
    asc(unsafe { &*tm }, &mut b);
    if size < b.n {
        return crate::errno::EINVAL;
    }
    unsafe { core::ptr::copy_nonoverlapping(b.b.as_ptr(), buf, b.n) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn _ctime64(t: *const i64) -> *mut u8 {
    let tm = unsafe { _localtime64(t) };
    if tm.is_null() { tm as *mut u8 } else { unsafe { asctime(tm) } }
}

#[no_mangle]
pub unsafe extern "C" fn ctime(t: *const i64) -> *mut u8 {
    unsafe { _ctime64(t) }
}

#[no_mangle]
pub unsafe extern "C" fn _ctime64_s(buf: *mut u8, size: usize, t: *const i64) -> i32 {
    let mut tm = Tm::default();
    let r = unsafe { into(&mut tm, *t) };
    if r != 0 {
        return r;
    }
    unsafe { asctime_s(buf, size, &tm) }
}

/// One strftime directive into `out` (ASCII).
fn directive(out: &mut Buf<64>, c: u8, tm: &Tm) {
    let y = tm.year + 1900;
    let h12 = if tm.hour % 12 == 0 { 12 } else { tm.hour % 12 };
    let _ = match c {
        b'a' => write!(out, "{}", &name(&DAYS, tm.wday)[..3]),
        b'A' => write!(out, "{}", name(&DAYS, tm.wday)),
        b'b' | b'h' => write!(out, "{}", &name(&MONTHS, tm.mon)[..3]),
        b'B' => write!(out, "{}", name(&MONTHS, tm.mon)),
        b'c' => write!(out, "{:02}/{:02}/{:02} {:02}:{:02}:{:02}", tm.mon + 1, tm.mday, y % 100, tm.hour, tm.min, tm.sec),
        b'C' => write!(out, "{:02}", y / 100),
        b'd' => write!(out, "{:02}", tm.mday),
        b'D' | b'x' => write!(out, "{:02}/{:02}/{:02}", tm.mon + 1, tm.mday, y % 100),
        b'e' => write!(out, "{:2}", tm.mday),
        b'F' => write!(out, "{}-{:02}-{:02}", y, tm.mon + 1, tm.mday),
        b'H' => write!(out, "{:02}", tm.hour),
        b'I' => write!(out, "{:02}", h12),
        b'j' => write!(out, "{:03}", tm.yday + 1),
        b'm' => write!(out, "{:02}", tm.mon + 1),
        b'M' => write!(out, "{:02}", tm.min),
        b'n' => write!(out, "\n"),
        b'p' => write!(out, "{}", if tm.hour < 12 { "AM" } else { "PM" }),
        b'r' => write!(out, "{:02}:{:02}:{:02} {}", h12, tm.min, tm.sec, if tm.hour < 12 { "AM" } else { "PM" }),
        b'R' => write!(out, "{:02}:{:02}", tm.hour, tm.min),
        b'S' => write!(out, "{:02}", tm.sec),
        b't' => write!(out, "\t"),
        b'T' | b'X' => write!(out, "{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec),
        b'u' => write!(out, "{}", if tm.wday == 0 { 7 } else { tm.wday }),
        b'w' => write!(out, "{}", tm.wday),
        b'U' => write!(out, "{:02}", (tm.yday + 7 - tm.wday) / 7),
        b'W' => write!(out, "{:02}", (tm.yday + 7 - (tm.wday + 6) % 7) / 7),
        b'G' | b'g' | b'V' => {
            let (iy, week) = iso_week(tm);
            match c {
                b'G' => write!(out, "{}", iy),
                b'g' => write!(out, "{:02}", iy % 100),
                _ => write!(out, "{:02}", week),
            }
        }
        b'y' => write!(out, "{:02}", y % 100),
        b'Y' => write!(out, "{}", y),
        b'z' => write!(out, "+0000"),
        b'Z' => write!(out, "UTC"),
        b'%' => write!(out, "%"),
        _ => Ok(()),
    };
}

/// ISO 8601 week-based year and week number.
fn iso_week(tm: &Tm) -> (i32, i32) {
    let y = tm.year + 1900;
    let wday = (tm.wday + 6) % 7; // Monday = 0
    let week = (tm.yday - wday + 10) / 7;
    if week < 1 {
        let prev = y - 1;
        let prev_days = if is_leap(prev as i64) { 366 } else { 365 };
        let t = Tm { yday: tm.yday + prev_days, year: tm.year - 1, ..*tm };
        return (prev, iso_week(&t).1);
    }
    let days = if is_leap(y as i64) { 366 } else { 365 };
    if week == 53 && tm.yday - wday + 3 >= days {
        return (y + 1, 1);
    }
    (y, week)
}

/// strftime over narrow or wide units.
unsafe fn format_time<C: crate::string::Ch>(out: *mut C, max: usize, fmt: *const C, tm: *const Tm) -> usize {
    let tm = unsafe { &*tm };
    let mut n = 0;
    let mut i = 0;
    let at = |i: usize| unsafe { (*fmt.add(i)).to_u32() };
    let put = |c: u32, n: &mut usize| -> bool {
        if *n + 1 >= max {
            return false;
        }
        unsafe { *out.add(*n) = C::from_u32(c) };
        *n += 1;
        true
    };
    while at(i) != 0 {
        let c = at(i);
        i += 1;
        if c != b'%' as u32 {
            if !put(c, &mut n) {
                return 0;
            }
            continue;
        }
        if at(i) == b'#' as u32 || at(i) == b'E' as u32 || at(i) == b'O' as u32 {
            i += 1;
        }
        let d = at(i);
        if d == 0 {
            break;
        }
        i += 1;
        let mut b = Buf { b: [0; 64], n: 0 };
        directive(&mut b, d as u8, tm);
        for &x in &b.b[..b.n] {
            if !put(x as u32, &mut n) {
                return 0;
            }
        }
    }
    if max == 0 {
        return 0;
    }
    unsafe { *out.add(n) = C::default() };
    n
}

#[no_mangle]
pub unsafe extern "C" fn strftime(out: *mut u8, max: usize, fmt: *const u8, tm: *const Tm) -> usize {
    unsafe { format_time(out, max, fmt, tm) }
}

#[no_mangle]
pub unsafe extern "C" fn _strftime_l(out: *mut u8, max: usize, fmt: *const u8, tm: *const Tm, _l: usize) -> usize {
    unsafe { format_time(out, max, fmt, tm) }
}

#[no_mangle]
pub unsafe extern "C" fn wcsftime(out: *mut u16, max: usize, fmt: *const u16, tm: *const Tm) -> usize {
    unsafe { format_time(out, max, fmt, tm) }
}

#[no_mangle]
pub unsafe extern "C" fn _Strftime(out: *mut u8, max: usize, fmt: *const u8, tm: *const Tm, _data: usize) -> usize {
    unsafe { format_time(out, max, fmt, tm) }
}

/// "MM/DD/YY" and "HH:MM:SS".
#[no_mangle]
pub unsafe extern "C" fn _strdate(buf: *mut u8) -> *mut u8 {
    let tm = broken_down(now()).unwrap_or_default();
    unsafe { format_time(buf, 9, b"%m/%d/%y\0".as_ptr(), &tm) };
    buf
}

#[no_mangle]
pub unsafe extern "C" fn _strtime(buf: *mut u8) -> *mut u8 {
    let tm = broken_down(now()).unwrap_or_default();
    unsafe { format_time(buf, 9, b"%H:%M:%S\0".as_ptr(), &tm) };
    buf
}
