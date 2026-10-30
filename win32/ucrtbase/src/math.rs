//! The math library (from the libm crate), floating-point classification
//! and control, and integer helpers.

macro_rules! unary {
    ($($n:ident / $f:ident),* $(,)?) => {$(
        #[no_mangle]
        pub extern "C" fn $n(x: f64) -> f64 { libm::$n(x) }
        #[no_mangle]
        pub extern "C" fn $f(x: f32) -> f32 { libm::$f(x) }
    )*};
}

macro_rules! binary {
    ($($n:ident / $f:ident),* $(,)?) => {$(
        #[no_mangle]
        pub extern "C" fn $n(x: f64, y: f64) -> f64 { libm::$n(x, y) }
        #[no_mangle]
        pub extern "C" fn $f(x: f32, y: f32) -> f32 { libm::$f(x, y) }
    )*};
}

unary! {
    sin / sinf, cos / cosf, tan / tanf, asin / asinf, acos / acosf, atan / atanf,
    sinh / sinhf, cosh / coshf, tanh / tanhf, asinh / asinhf, acosh / acoshf, atanh / atanhf,
    exp / expf, exp2 / exp2f, expm1 / expm1f, log / logf, log10 / log10f, log2 / log2f, log1p / log1pf,
    sqrt / sqrtf, cbrt / cbrtf, fabs / fabsf, floor / floorf, ceil / ceilf, trunc / truncf,
    round / roundf, rint / rintf, erf / erff, erfc / erfcf, tgamma / tgammaf, lgamma / lgammaf,
}

binary! {
    atan2 / atan2f, pow / powf, hypot / hypotf, fmod / fmodf, remainder / remainderf,
    nextafter / nextafterf, copysign / copysignf, fmin / fminf, fmax / fmaxf, fdim / fdimf,
}

#[no_mangle]
pub extern "C" fn nearbyint(x: f64) -> f64 {
    libm::rint(x)
}

#[no_mangle]
pub extern "C" fn nearbyintf(x: f32) -> f32 {
    libm::rintf(x)
}

#[no_mangle]
pub extern "C" fn fma(x: f64, y: f64, z: f64) -> f64 {
    libm::fma(x, y, z)
}

#[no_mangle]
pub extern "C" fn fmaf(x: f32, y: f32, z: f32) -> f32 {
    libm::fmaf(x, y, z)
}

#[no_mangle]
pub extern "C" fn ldexp(x: f64, e: i32) -> f64 {
    libm::ldexp(x, e)
}

#[no_mangle]
pub extern "C" fn ldexpf(x: f32, e: i32) -> f32 {
    libm::ldexpf(x, e)
}

#[no_mangle]
pub extern "C" fn scalbn(x: f64, e: i32) -> f64 {
    libm::scalbn(x, e)
}

#[no_mangle]
pub extern "C" fn scalbnf(x: f32, e: i32) -> f32 {
    libm::scalbnf(x, e)
}

#[no_mangle]
pub extern "C" fn scalbln(x: f64, e: i32) -> f64 {
    libm::scalbn(x, e)
}

#[no_mangle]
pub extern "C" fn _scalb(x: f64, e: i32) -> f64 {
    libm::scalbn(x, e)
}

#[no_mangle]
pub extern "C" fn ilogb(x: f64) -> i32 {
    libm::ilogb(x)
}

#[no_mangle]
pub extern "C" fn ilogbf(x: f32) -> i32 {
    libm::ilogbf(x)
}

#[no_mangle]
pub extern "C" fn logb(x: f64) -> f64 {
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if !x.is_finite() {
        return x.abs();
    }
    libm::ilogb(x) as f64
}

#[no_mangle]
pub extern "C" fn _logb(x: f64) -> f64 {
    logb(x)
}

#[no_mangle]
pub unsafe extern "C" fn frexp(x: f64, e: *mut i32) -> f64 {
    let (m, k) = libm::frexp(x);
    unsafe { *e = k };
    m
}

#[no_mangle]
pub unsafe extern "C" fn frexpf(x: f32, e: *mut i32) -> f32 {
    let (m, k) = libm::frexpf(x);
    unsafe { *e = k };
    m
}

#[no_mangle]
pub unsafe extern "C" fn modf(x: f64, ip: *mut f64) -> f64 {
    let (f, i) = libm::modf(x);
    unsafe { *ip = i };
    f
}

#[no_mangle]
pub unsafe extern "C" fn modff(x: f32, ip: *mut f32) -> f32 {
    let (f, i) = libm::modff(x);
    unsafe { *ip = i };
    f
}

#[no_mangle]
pub unsafe extern "C" fn remquo(x: f64, y: f64, q: *mut i32) -> f64 {
    let (r, k) = libm::remquo(x, y);
    unsafe { *q = k };
    r
}

#[no_mangle]
pub unsafe extern "C" fn remquof(x: f32, y: f32, q: *mut i32) -> f32 {
    let (r, k) = libm::remquof(x, y);
    unsafe { *q = k };
    r
}

#[no_mangle]
pub extern "C" fn lround(x: f64) -> i32 {
    libm::round(x) as i32
}

#[no_mangle]
pub extern "C" fn lroundf(x: f32) -> i32 {
    libm::roundf(x) as i32
}

#[no_mangle]
pub extern "C" fn llround(x: f64) -> i64 {
    libm::round(x) as i64
}

#[no_mangle]
pub extern "C" fn llroundf(x: f32) -> i64 {
    libm::roundf(x) as i64
}

#[no_mangle]
pub extern "C" fn lrint(x: f64) -> i32 {
    libm::rint(x) as i32
}

#[no_mangle]
pub extern "C" fn lrintf(x: f32) -> i32 {
    libm::rintf(x) as i32
}

#[no_mangle]
pub extern "C" fn llrint(x: f64) -> i64 {
    libm::rint(x) as i64
}

#[no_mangle]
pub extern "C" fn llrintf(x: f32) -> i64 {
    libm::rintf(x) as i64
}

#[no_mangle]
pub extern "C" fn nan(_tag: *const u8) -> f64 {
    f64::NAN
}

#[no_mangle]
pub extern "C" fn nanf(_tag: *const u8) -> f32 {
    f32::NAN
}

#[no_mangle]
pub extern "C" fn _hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

#[no_mangle]
pub extern "C" fn _hypotf(x: f32, y: f32) -> f32 {
    libm::hypotf(x, y)
}

#[no_mangle]
pub extern "C" fn _copysign(x: f64, y: f64) -> f64 {
    libm::copysign(x, y)
}

#[no_mangle]
pub extern "C" fn _copysignf(x: f32, y: f32) -> f32 {
    libm::copysignf(x, y)
}

#[no_mangle]
pub extern "C" fn _chgsign(x: f64) -> f64 {
    -x
}

#[no_mangle]
pub extern "C" fn _nextafter(x: f64, y: f64) -> f64 {
    libm::nextafter(x, y)
}

// -----| classification |-----

const FP_INFINITE: i16 = 1;
const FP_NAN: i16 = 2;
const FP_NORMAL: i16 = -1;
const FP_SUBNORMAL: i16 = -2;
const FP_ZERO: i16 = 0;

fn class64(x: f64) -> i16 {
    match x.classify() {
        core::num::FpCategory::Infinite => FP_INFINITE,
        core::num::FpCategory::Nan => FP_NAN,
        core::num::FpCategory::Normal => FP_NORMAL,
        core::num::FpCategory::Subnormal => FP_SUBNORMAL,
        core::num::FpCategory::Zero => FP_ZERO,
    }
}

/// UCRT's fpclassify / isnan / signbit macros call these.
#[no_mangle]
pub extern "C" fn _dclass(x: f64) -> i16 {
    class64(x)
}

#[no_mangle]
pub extern "C" fn _ldclass(x: f64) -> i16 {
    class64(x)
}

#[no_mangle]
pub extern "C" fn _fdclass(x: f32) -> i16 {
    class64(x as f64)
}

#[no_mangle]
pub extern "C" fn _dsign(x: f64) -> i32 {
    x.is_sign_negative() as i32 * 0x8000
}

#[no_mangle]
pub extern "C" fn _ldsign(x: f64) -> i32 {
    _dsign(x)
}

#[no_mangle]
pub extern "C" fn _fdsign(x: f32) -> i32 {
    x.is_sign_negative() as i32 * 0x8000
}

#[no_mangle]
pub unsafe extern "C" fn _dtest(x: *const f64) -> i16 {
    class64(unsafe { *x })
}

#[no_mangle]
pub unsafe extern "C" fn _ldtest(x: *const f64) -> i16 {
    class64(unsafe { *x })
}

#[no_mangle]
pub unsafe extern "C" fn _fdtest(x: *const f32) -> i16 {
    class64(unsafe { *x } as f64)
}

#[no_mangle]
pub extern "C" fn _dpcomp(x: f64, y: f64) -> i32 {
    // _FP_LT 1, _FP_EQ 2, _FP_GT 4; unordered 0.
    if x < y {
        1
    } else if x == y {
        2
    } else if x > y {
        4
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn _fdpcomp(x: f32, y: f32) -> i32 {
    _dpcomp(x as f64, y as f64)
}

#[no_mangle]
pub extern "C" fn _isnan(x: f64) -> i32 {
    x.is_nan() as i32
}

#[no_mangle]
pub extern "C" fn _isnanf(x: f32) -> i32 {
    x.is_nan() as i32
}

#[no_mangle]
pub extern "C" fn _finite(x: f64) -> i32 {
    x.is_finite() as i32
}

#[no_mangle]
pub extern "C" fn _finitef(x: f32) -> i32 {
    x.is_finite() as i32
}

#[no_mangle]
pub extern "C" fn _fpclass(x: f64) -> i32 {
    let neg = x.is_sign_negative();
    match x.classify() {
        core::num::FpCategory::Nan => if x.to_bits() & (1 << 51) != 0 { 2 } else { 1 },
        core::num::FpCategory::Infinite => if neg { 4 } else { 0x200 },
        core::num::FpCategory::Normal => if neg { 8 } else { 0x100 },
        core::num::FpCategory::Subnormal => if neg { 0x10 } else { 0x80 },
        core::num::FpCategory::Zero => if neg { 0x20 } else { 0x40 },
    }
}

// -----| floating-point control |-----

/// _CW_DEFAULT: round to nearest, all exceptions masked.
static mut CONTROL: u32 = 0x0008_001F;

#[no_mangle]
pub extern "C" fn _controlfp(new: u32, mask: u32) -> u32 {
    unsafe {
        CONTROL = (CONTROL & !mask) | (new & mask);
        CONTROL
    }
}

#[no_mangle]
pub extern "C" fn _control87(new: u32, mask: u32) -> u32 {
    _controlfp(new, mask)
}

#[no_mangle]
pub unsafe extern "C" fn _controlfp_s(current: *mut u32, new: u32, mask: u32) -> i32 {
    let v = _controlfp(new, mask);
    if !current.is_null() {
        unsafe { *current = v };
    }
    0
}

#[no_mangle]
pub extern "C" fn _clearfp() -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn _statusfp() -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn _fpreset() {}

#[no_mangle]
pub extern "C" fn _set_FMA3_enable(_flag: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fegetround() -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fesetround(mode: i32) -> i32 {
    (mode != 0) as i32
}

#[no_mangle]
pub extern "C" fn feclearexcept(_e: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fetestexcept(_e: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn feraiseexcept(_e: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fegetenv(_env: *mut u64) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fesetenv(_env: *const u64) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn feholdexcept(_env: *mut u64) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn feupdateenv(_env: *const u64) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fegetexceptflag(_f: *mut u32, _e: i32) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn fesetexceptflag(_f: *const u32, _e: i32) -> i32 {
    0
}

// -----| integers |-----

#[no_mangle]
pub extern "C" fn abs(x: i32) -> i32 {
    x.wrapping_abs()
}

#[no_mangle]
pub extern "C" fn labs(x: i32) -> i32 {
    x.wrapping_abs()
}

#[no_mangle]
pub extern "C" fn llabs(x: i64) -> i64 {
    x.wrapping_abs()
}

#[no_mangle]
pub extern "C" fn _abs64(x: i64) -> i64 {
    x.wrapping_abs()
}

#[no_mangle]
pub extern "C" fn imaxabs(x: i64) -> i64 {
    x.wrapping_abs()
}

#[repr(C)]
pub struct Div {
    quot: i32,
    rem: i32,
}

#[repr(C)]
pub struct LlDiv {
    quot: i64,
    rem: i64,
}

#[no_mangle]
pub extern "C" fn div(a: i32, b: i32) -> Div {
    Div { quot: a.wrapping_div(b), rem: a.wrapping_rem(b) }
}

#[no_mangle]
pub extern "C" fn ldiv(a: i32, b: i32) -> Div {
    div(a, b)
}

#[no_mangle]
pub extern "C" fn lldiv(a: i64, b: i64) -> LlDiv {
    LlDiv { quot: a.wrapping_div(b), rem: a.wrapping_rem(b) }
}

#[no_mangle]
pub extern "C" fn imaxdiv(a: i64, b: i64) -> LlDiv {
    lldiv(a, b)
}

#[no_mangle]
pub extern "C" fn _rotl(x: u32, n: i32) -> u32 {
    x.rotate_left(n as u32 & 31)
}

#[no_mangle]
pub extern "C" fn _rotr(x: u32, n: i32) -> u32 {
    x.rotate_right(n as u32 & 31)
}

#[no_mangle]
pub extern "C" fn _lrotl(x: u32, n: i32) -> u32 {
    _rotl(x, n)
}

#[no_mangle]
pub extern "C" fn _lrotr(x: u32, n: i32) -> u32 {
    _rotr(x, n)
}

#[no_mangle]
pub extern "C" fn _rotl64(x: u64, n: i32) -> u64 {
    x.rotate_left(n as u32 & 63)
}

#[no_mangle]
pub extern "C" fn _rotr64(x: u64, n: i32) -> u64 {
    x.rotate_right(n as u32 & 63)
}

#[no_mangle]
pub extern "C" fn _byteswap_ushort(x: u16) -> u16 {
    x.swap_bytes()
}

#[no_mangle]
pub extern "C" fn _byteswap_ulong(x: u32) -> u32 {
    x.swap_bytes()
}

#[no_mangle]
pub extern "C" fn _byteswap_uint64(x: u64) -> u64 {
    x.swap_bytes()
}
