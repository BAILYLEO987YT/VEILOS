//! What the kernel's font engine (src/font.rs, compiled into gdi32) needs
//! from the kernel's 2D renderer. gdi32 draws glyph coverage itself, so the
//! engine's own text drawing (`draw_text`, used by its demo) draws nothing.

pub fn isqrt_u64(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let bits = 64 - n.leading_zeros();
    let mut x = 1u64 << ((bits + 1) / 2);
    loop {
        let y = (x + n / x) >> 1;
        if y >= x {
            return x;
        }
        x = y;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color32 {
    pub const WHITE: Color32 = Color32 { r: 255, g: 255, b: 255, a: 255 };
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color32 { r, g, b, a }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color32 { r, g, b, a: 255 }
    }
}

pub struct Canvas<'a>(core::marker::PhantomData<&'a mut [u32]>);

impl Canvas<'_> {
    #[allow(clippy::too_many_arguments)]
    pub fn blit_mask(&mut self, _mask: &[u8], _w: i32, _h: i32, _x: i32, _y: i32, _color: Color32, _opacity: u8) {}
}
