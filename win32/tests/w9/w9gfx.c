/* G2 test: text and images. Prints what DIB sections, DIB copies,
 * blending, palettes, resource bitmaps and icons, fonts and DrawText give
 * back. Font checks print relations that hold for any font (VEILOS has
 * other faces than Windows).
 *
 *   w9gfx        the scripted run, compared with Windows' output
 *   w9gfx wait   a window showing fonts, images and icons, open until closed
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define IDB_TEST 101
#define IDI_TEST 102

static HINSTANCE inst;

static HDC mem_dc(HBITMAP bmp, HGDIOBJ *old)
{
    HDC dc = CreateCompatibleDC(NULL);
    *old = SelectObject(dc, bmp);
    return dc;
}

static void done_dc(HDC dc, HGDIOBJ old)
{
    SelectObject(dc, old);
    DeleteDC(dc);
}

static void make_info(BITMAPINFO *bi, int w, int h, int bpp)
{
    memset(bi, 0, sizeof *bi);
    bi->bmiHeader.biSize = sizeof bi->bmiHeader;
    bi->bmiHeader.biWidth = w;
    bi->bmiHeader.biHeight = h;
    bi->bmiHeader.biPlanes = 1;
    bi->bmiHeader.biBitCount = (WORD)bpp;
}

static void dib_sections(void)
{
    struct { BITMAPINFOHEADER h; RGBQUAD c[256]; } b8;
    struct { BITMAPINFOHEADER h; DWORD m[3]; } b16;
    BITMAPINFO bi;
    DIBSECTION ds;
    DWORD *p32;
    BYTE *p8, *p24;
    WORD *p16;
    HGDIOBJ old;
    HDC dc;
    HBITMAP bmp, b2;
    RECT r;
    HBRUSH green;

    /* 32bpp bottom-up: bits[0] is the bottom-left pixel. */
    make_info(&bi, 4, 4, 32);
    bmp = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, (void **)&p32, NULL, 0);
    p32[0] = 0x00112233;
    dc = mem_dc(bmp, &old);
    printf("dib32 bottom-left %06lx top-left %06lx\n", GetPixel(dc, 0, 3), GetPixel(dc, 0, 0));
    SetPixel(dc, 3, 0, RGB(1, 2, 3));
    printf("dib32 SetPixel top-right -> bits[15] %08lx\n", p32[15]);
    GetObject(bmp, sizeof ds, &ds);
    printf("DIBSECTION %d %dx%d bpp %d bits ok %d biHeight %ld size %lu\n", GetObject(bmp, 0, NULL),
           (int)ds.dsBm.bmWidth, (int)ds.dsBm.bmHeight, ds.dsBm.bmBitsPixel, ds.dsBm.bmBits == p32,
           ds.dsBmih.biHeight, ds.dsBmih.biSizeImage);
    done_dc(dc, old);
    DeleteObject(bmp);

    /* 8bpp with a 4-color table: drawing picks the nearest color. */
    memset(&b8, 0, sizeof b8);
    b8.h = bi.bmiHeader;
    b8.h.biWidth = 4;
    b8.h.biHeight = -4;
    b8.h.biBitCount = 8;
    b8.h.biClrUsed = 4;
    b8.c[1].rgbRed = 255;
    b8.c[2].rgbGreen = 255;
    b8.c[3].rgbBlue = 255;
    bmp = CreateDIBSection(NULL, (BITMAPINFO *)&b8, DIB_RGB_COLORS, (void **)&p8, NULL, 0);
    dc = mem_dc(bmp, &old);
    SetPixel(dc, 1, 1, RGB(250, 10, 10));
    printf("dib8 SetPixel near-red -> index %d, GetPixel %06lx\n", p8[1 * 4 + 1], GetPixel(dc, 1, 1));
    green = CreateSolidBrush(RGB(0, 200, 0));
    SetRect(&r, 0, 2, 4, 4);
    FillRect(dc, &r, green);
    printf("dib8 FillRect green -> indices %d %d, row 0 %d\n", p8[2 * 4], p8[3 * 4 + 3], p8[0]);
    p8[0] = 3;
    printf("dib8 bits written -> GetPixel %06lx\n", GetPixel(dc, 0, 0));
    {
        RGBQUAD q[4];
        UINT n = GetDIBColorTable(dc, 0, 4, q);
        printf("color table %u: %02x%02x%02x\n", n, q[1].rgbRed, q[1].rgbGreen, q[1].rgbBlue);
    }
    done_dc(dc, old);
    DeleteObject(bmp);
    DeleteObject(green);

    /* 24bpp top-down. */
    make_info(&bi, 3, -2, 24);
    bmp = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, (void **)&p24, NULL, 0);
    p24[3] = 0x10; p24[4] = 0x20; p24[5] = 0x30;
    dc = mem_dc(bmp, &old);
    printf("dib24 (1,0) %06lx\n", GetPixel(dc, 1, 0));
    SetPixel(dc, 2, 1, RGB(0xAA, 0xBB, 0xCC));
    printf("dib24 SetPixel (2,1) -> %02x %02x %02x\n", p24[12 + 6], p24[12 + 7], p24[12 + 8]);
    done_dc(dc, old);
    DeleteObject(bmp);

    /* 16bpp 5-6-5. */
    memset(&b16, 0, sizeof b16);
    b16.h = bi.bmiHeader;
    b16.h.biWidth = 2;
    b16.h.biHeight = -1;
    b16.h.biBitCount = 16;
    b16.h.biCompression = BI_BITFIELDS;
    b16.m[0] = 0xF800; b16.m[1] = 0x07E0; b16.m[2] = 0x001F;
    bmp = CreateDIBSection(NULL, (BITMAPINFO *)&b16, DIB_RGB_COLORS, (void **)&p16, NULL, 0);
    dc = mem_dc(bmp, &old);
    SetPixel(dc, 0, 0, RGB(255, 0, 0));
    SetPixel(dc, 1, 0, RGB(0, 255, 255));
    printf("dib16 565 -> %04x %04x\n", p16[0], p16[1]);
    done_dc(dc, old);
    DeleteObject(bmp);

    /* A DIB section as a BitBlt source. */
    make_info(&bi, 2, 2, 32);
    bmp = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, (void **)&p32, NULL, 0);
    p32[0] = 0x0000FF; p32[1] = 0x00FF00; p32[2] = 0xFF0000; p32[3] = 0xFFFFFF;
    b2 = CreateCompatibleBitmap(GetDC(NULL), 2, 2);
    {
        HGDIOBJ o2;
        HDC d1 = mem_dc(bmp, &old), d2 = mem_dc(b2, &o2);
        BitBlt(d2, 0, 0, 2, 2, d1, 0, 0, SRCCOPY);
        printf("blt from dib32: %06lx %06lx %06lx %06lx\n", GetPixel(d2, 0, 0), GetPixel(d2, 1, 0), GetPixel(d2, 0, 1), GetPixel(d2, 1, 1));
        done_dc(d2, o2);
        done_dc(d1, old);
    }
    DeleteObject(b2);
    DeleteObject(bmp);
}

static void dib_copies(void)
{
    BYTE src[2 * 2 * 4] = { 0xFF, 0, 0, 0,  0, 0xFF, 0, 0,   0, 0, 0xFF, 0,  0x80, 0x80, 0x80, 0 };
    BYTE src24[2 * 8] = { 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0 };
    BYTE out[8 * 8 * 4];
    BITMAPINFO bi;
    struct { BITMAPINFOHEADER h; DWORD m[3]; } q;
    HBITMAP bmp = CreateCompatibleBitmap(GetDC(NULL), 8, 8);
    HGDIOBJ old;
    HDC dc = mem_dc(bmp, &old);
    RECT all = { 0, 0, 8, 8 };
    int r;

    FillRect(dc, &all, (HBRUSH)GetStockObject(WHITE_BRUSH));
    /* 2x2 bottom-up 32bpp: row 0 of the bits is the bottom row. */
    make_info(&bi, 2, 2, 32);
    r = StretchDIBits(dc, 0, 0, 4, 4, 0, 0, 2, 2, src, &bi, DIB_RGB_COLORS, SRCCOPY);
    printf("StretchDIBits %d: %06lx %06lx %06lx %06lx\n", r, GetPixel(dc, 0, 0), GetPixel(dc, 3, 0), GetPixel(dc, 0, 3), GetPixel(dc, 3, 3));
    r = StretchDIBits(dc, 4, 0, -2, 2, 0, 0, 2, 2, src, &bi, DIB_RGB_COLORS, SRCCOPY);
    printf("mirrored %06lx %06lx\n", GetPixel(dc, 2, 0), GetPixel(dc, 3, 0));
    /* 2x2 bottom-up 24bpp (rows padded to 8 bytes) at (5,5). */
    make_info(&bi, 2, 2, 24);
    r = SetDIBitsToDevice(dc, 5, 5, 2, 2, 0, 0, 0, 2, src24, &bi, DIB_RGB_COLORS);
    printf("SetDIBitsToDevice %d: %06lx %06lx %06lx\n", r, GetPixel(dc, 5, 6), GetPixel(dc, 6, 6), GetPixel(dc, 5, 5));
    done_dc(dc, old);

    memset(&q, 0, sizeof q);
    q.h.biSize = sizeof q.h;
    r = GetDIBits(GetDC(NULL), bmp, 0, 0, NULL, (BITMAPINFO *)&q, DIB_RGB_COLORS);
    printf("GetDIBits query %d: %ldx%ld bpp %d compression %lu size %lu\n", r != 0, q.h.biWidth, q.h.biHeight, q.h.biBitCount,
           q.h.biCompression, q.h.biSizeImage);
    make_info(&bi, 8, -8, 24);
    r = GetDIBits(GetDC(NULL), bmp, 0, 8, out, &bi, DIB_RGB_COLORS);
    printf("GetDIBits 24 top-down %d: %02x %02x %02x | %02x %02x %02x\n", r, out[0], out[1], out[2], out[3 * 3], out[3 * 3 + 1], out[3 * 3 + 2]);
    {
        BYTE eight[4] = { 1, 0, 0, 0 };
        struct { BITMAPINFOHEADER h; RGBQUAD c[2]; } b8;
        HBITMAP d;
        memset(&b8, 0, sizeof b8);
        b8.h.biSize = sizeof b8.h;
        b8.h.biWidth = 1;
        b8.h.biHeight = 1;
        b8.h.biPlanes = 1;
        b8.h.biBitCount = 8;
        b8.h.biClrUsed = 2;
        b8.c[1].rgbRed = 0x12; b8.c[1].rgbGreen = 0x34; b8.c[1].rgbBlue = 0x56;
        d = CreateDIBitmap(GetDC(NULL), &b8.h, CBM_INIT, eight, (BITMAPINFO *)&b8, DIB_RGB_COLORS);
        dc = mem_dc(d, &old);
        printf("CreateDIBitmap pixel %06lx\n", GetPixel(dc, 0, 0));
        done_dc(dc, old);
        DeleteObject(d);
    }
    DeleteObject(bmp);
}

static void blending(void)
{
    BITMAPINFO bi;
    DWORD *src, *dst;
    HBITMAP sb, db;
    HGDIOBJ so, dob;
    HDC sdc, ddc;
    BLENDFUNCTION bf = { AC_SRC_OVER, 0, 255, AC_SRC_ALPHA };
    HPALETTE pal;
    struct { WORD v, n; PALETTEENTRY e[3]; } lp = { 0x300, 3, { { 0, 0, 0, 0 }, { 255, 0, 0, 0 }, { 0, 0, 255, 0 } } };

    make_info(&bi, 2, -1, 32);
    sb = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, (void **)&src, NULL, 0);
    db = CreateDIBSection(NULL, &bi, DIB_RGB_COLORS, (void **)&dst, NULL, 0);
    sdc = mem_dc(sb, &so);
    ddc = mem_dc(db, &dob);
    /* Premultiplied half-transparent red over white, and a clear pixel. */
    src[0] = 0x80800000; src[1] = 0;
    dst[0] = dst[1] = 0x00FFFFFF;
    AlphaBlend(ddc, 0, 0, 2, 1, sdc, 0, 0, 2, 1, bf);
    printf("AlphaBlend per-pixel: %08lx %08lx\n", dst[0], dst[1]);
    dst[0] = dst[1] = 0x00FFFFFF;
    src[0] = 0; src[1] = 0;
    bf.SourceConstantAlpha = 128;
    bf.AlphaFormat = 0;
    AlphaBlend(ddc, 0, 0, 1, 1, sdc, 0, 0, 1, 1, bf);
    printf("AlphaBlend constant: %06lx\n", dst[0] & 0xFFFFFF);
    src[0] = 0x00FF00FF; src[1] = 0x00FF0000;
    dst[0] = dst[1] = 0x00FFFFFF;
    TransparentBlt(ddc, 0, 0, 2, 1, sdc, 0, 0, 2, 1, RGB(255, 0, 255));
    printf("TransparentBlt: %06lx %06lx\n", dst[0] & 0xFFFFFF, dst[1] & 0xFFFFFF);
    done_dc(sdc, so);
    done_dc(ddc, dob);
    DeleteObject(sb);
    DeleteObject(db);

    pal = CreatePalette((LOGPALETTE *)&lp);
    printf("palette entries %u, nearest to dark red %u, to blue %u\n", GetPaletteEntries(pal, 0, 0, NULL),
           GetNearestPaletteIndex(pal, RGB(200, 20, 0)), GetNearestPaletteIndex(pal, RGB(10, 10, 240)));
    DeleteObject(pal);
}

static void resources(void)
{
    HBITMAP bmp = LoadBitmap(inst, MAKEINTRESOURCE(IDB_TEST));
    BITMAP bm;
    DIBSECTION ds;
    ICONINFO ii;
    HGDIOBJ old;
    HDC dc;
    HICON ico;
    HBITMAP canvas;
    RECT all = { 0, 0, 40, 40 };

    GetObject(bmp, sizeof bm, &bm);
    dc = mem_dc(bmp, &old);
    printf("LoadBitmap %dx%d bpp %d: (2,3) %06lx (7,0) %06lx\n", (int)bm.bmWidth, (int)bm.bmHeight, bm.bmBitsPixel,
           GetPixel(dc, 2, 3), GetPixel(dc, 7, 0));
    done_dc(dc, old);
    DeleteObject(bmp);
    bmp = (HBITMAP)LoadImage(inst, MAKEINTRESOURCE(IDB_TEST), IMAGE_BITMAP, 0, 0, LR_CREATEDIBSECTION);
    printf("LoadImage DIB section: %d bpp %d\n", GetObject(bmp, sizeof ds, &ds) == sizeof ds, ds.dsBmih.biBitCount);
    DeleteObject(bmp);

    ico = LoadIcon(inst, MAKEINTRESOURCE(IDI_TEST));
    GetIconInfo(ico, &ii);
    GetObject(ii.hbmColor, sizeof bm, &bm);
    printf("LoadIcon: icon %d, color %dx%d", ii.fIcon, (int)bm.bmWidth, (int)bm.bmHeight);
    GetObject(ii.hbmMask, sizeof bm, &bm);
    printf(", mask %dx%d bpp %d\n", (int)bm.bmWidth, (int)bm.bmHeight, bm.bmBitsPixel);
    DeleteObject(ii.hbmColor);
    DeleteObject(ii.hbmMask);

    canvas = CreateCompatibleBitmap(GetDC(NULL), 40, 40);
    dc = mem_dc(canvas, &old);
    FillRect(dc, &all, (HBRUSH)GetStockObject(WHITE_BRUSH));
    DrawIconEx(dc, 0, 0, ico, 0, 0, 0, NULL, DI_NORMAL);
    printf("DrawIconEx: opaque %06lx clear %06lx half %06lx\n", GetPixel(dc, 5, 5), GetPixel(dc, 25, 5), GetPixel(dc, 20, 5));
    done_dc(dc, old);
    DeleteObject(canvas);

    ico = (HICON)LoadImage(inst, MAKEINTRESOURCE(IDI_TEST), IMAGE_ICON, 16, 16, 0);
    GetIconInfo(ico, &ii);
    GetObject(ii.hbmColor, sizeof bm, &bm);
    printf("LoadImage 16x16 icon: %dx%d\n", (int)bm.bmWidth, (int)bm.bmHeight);
    DestroyIcon(ico);

    ico = LoadIcon(NULL, IDI_APPLICATION);
    GetIconInfo(ico, &ii);
    GetObject(ii.hbmColor, sizeof bm, &bm);
    printf("IDI_APPLICATION %d: %dx%d\n", ico != NULL, (int)bm.bmWidth, (int)bm.bmHeight);
    printf("IDI_ERROR twice same %d\n", LoadIcon(NULL, IDI_ERROR) == LoadIcon(NULL, IDI_ERROR));
    GetIconInfo(LoadCursor(NULL, IDC_ARROW), &ii);
    printf("IDC_ARROW: icon %d\n", ii.fIcon);
}

static void fonts(void)
{
    HDC dc = CreateCompatibleDC(NULL);
    HFONT arial = CreateFontA(-20, 0, 0, 0, FW_BOLD, 0, 0, 0, ANSI_CHARSET, 0, 0, 0, VARIABLE_PITCH | FF_SWISS, "Arial");
    HFONT courier = CreateFontA(-16, 0, 0, 0, FW_NORMAL, 0, 0, 0, ANSI_CHARSET, 0, 0, 0, FIXED_PITCH | FF_MODERN, "Courier New");
    HGDIOBJ old = SelectObject(dc, arial);
    TEXTMETRICA tm;
    SIZE a, b, w;
    LOGFONTA lf;
    char face[64];
    int fit, cw;
    RECT r;
    char text[64];

    GetTextMetricsA(dc, &tm);
    printf("arial: height = ascent + descent %d, height >= 20 %d, weight %ld, TrueType %d, variable %d\n",
           tm.tmHeight == tm.tmAscent + tm.tmDescent, tm.tmHeight >= 20, tm.tmWeight, (tm.tmPitchAndFamily & TMPF_TRUETYPE) != 0,
           (tm.tmPitchAndFamily & TMPF_FIXED_PITCH) != 0);
    GetTextFaceA(dc, sizeof face, face);
    printf("face \"%s\"\n", face);
    GetTextExtentPoint32A(dc, "iiii", 4, &a);
    GetTextExtentPoint32A(dc, "WWWW", 4, &b);
    GetTextExtentPoint32A(dc, "W", 1, &w);
    GetCharWidth32A(dc, 'W', 'W', &cw);
    printf("iiii narrower than WWWW %d, extent height = tmHeight %d, char width W = extent %d\n", a.cx < b.cx, b.cy == tm.tmHeight, cw == w.cx);
    GetTextExtentPoint32A(dc, "abc", 3, &a);
    GetTextExtentExPointA(dc, "abcdef", 6, a.cx, &fit, NULL, &b);
    printf("fit in width of abc: %d\n", fit);
    GetObjectA(arial, sizeof lf, &lf);
    printf("LOGFONT %ld %ld \"%s\"\n", lf.lfHeight, lf.lfWeight, lf.lfFaceName);

    SelectObject(dc, courier);
    GetTextMetricsA(dc, &tm);
    GetTextExtentPoint32A(dc, "iiii", 4, &a);
    GetTextExtentPoint32A(dc, "WWWW", 4, &b);
    printf("courier: fixed %d, iiii = WWWW %d\n", (tm.tmPitchAndFamily & TMPF_FIXED_PITCH) == 0, a.cx == b.cx);

    /* DrawText, against the font's own measurements. */
    SelectObject(dc, arial);
    GetTextMetricsA(dc, &tm);
    GetTextExtentPoint32A(dc, "Hello", 5, &a);
    SetRect(&r, 10, 10, 10, 10);
    fit = DrawTextA(dc, "Hello", -1, &r, DT_CALCRECT | DT_SINGLELINE);
    printf("CALCRECT single line: height %d, width = extent %d\n", fit == tm.tmHeight, r.right - r.left == a.cx);
    GetTextExtentPoint32A(dc, "aaa bbb", 7, &a);
    SetRect(&r, 0, 0, a.cx, 0);
    printf("wrap to aaa bbb: lines %d\n", DrawTextA(dc, "aaa bbb ccc", -1, &r, DT_CALCRECT | DT_WORDBREAK) / tm.tmHeight);
    GetTextExtentPoint32A(dc, "aaa", 3, &a);
    SetRect(&r, 0, 0, a.cx, 0);
    printf("wrap to aaa: lines %d\n", DrawTextA(dc, "aaa bbb ccc", -1, &r, DT_CALCRECT | DT_WORDBREAK) / tm.tmHeight);
    printf("two lines: %d\n", DrawTextA(dc, "one\ntwo", -1, &r, DT_CALCRECT) / tm.tmHeight);
    GetTextExtentPoint32A(dc, "File", 4, &a);
    SetRect(&r, 0, 0, 0, 0);
    DrawTextA(dc, "&File", -1, &r, DT_CALCRECT | DT_SINGLELINE);
    printf("prefix: width = extent of File %d\n", r.right == a.cx);
    strcpy(text, "abcdefghijklmnop");
    GetTextExtentPoint32A(dc, text, 16, &a);
    SetRect(&r, 0, 0, a.cx / 2, 100);
    DrawTextA(dc, text, -1, &r, DT_END_ELLIPSIS | DT_MODIFYSTRING | DT_SINGLELINE);
    GetTextExtentPoint32A(dc, text, (int)strlen(text), &b);
    printf("ellipsis: ends with ... %d, fits %d, shorter %d\n", strcmp(text + strlen(text) - 3, "...") == 0, b.cx <= r.right,
           strlen(text) < 16);
    SetRect(&r, 0, 0, 200, 100);
    printf("vcenter returns %d\n", DrawTextA(dc, "x", -1, &r, DT_SINGLELINE | DT_VCENTER) == (100 - tm.tmHeight) / 2 + tm.tmHeight);

    SelectObject(dc, old);
    DeleteObject(arial);
    DeleteObject(courier);
    DeleteDC(dc);
}

/* -----| the window (w9gfx wait) |----- */

static LRESULT CALLBACK proc(HWND hwnd, UINT m, WPARAM w, LPARAM l)
{
    if (m == WM_PAINT) {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(hwnd, &ps);
        const char *faces[] = { "Arial", "Times New Roman", "Courier New" };
        HBITMAP bmp = LoadBitmap(inst, MAKEINTRESOURCE(IDB_TEST));
        HDC mem = CreateCompatibleDC(dc);
        HGDIOBJ old = SelectObject(mem, bmp);
        RECT r = { 300, 20, 560, 150 };
        BITMAPINFO bi;
        DWORD grad[64 * 64];
        int i, y = 10;
        SetBkMode(dc, TRANSPARENT);
        for (i = 0; i < 3; i++) {
            HFONT f = CreateFontA(-18, 0, 0, 0, FW_NORMAL, 0, 0, 0, 0, 0, 0, 0, 0, faces[i]);
            HGDIOBJ of = SelectObject(dc, f);
            TextOutA(dc, 10, y, faces[i], (int)strlen(faces[i]));
            SelectObject(dc, of);
            DeleteObject(f);
            y += 26;
        }
        {
            HFONT f = CreateFontA(-22, 0, 0, 0, FW_BOLD, TRUE, TRUE, 0, 0, 0, 0, 0, 0, "Arial");
            HGDIOBJ of = SelectObject(dc, f);
            SetTextColor(dc, RGB(0, 90, 180));
            TextOutA(dc, 10, y, "Bold italic underline", 21);
            SetTextColor(dc, 0);
            SelectObject(dc, of);
            DeleteObject(f);
            y += 34;
        }
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        DrawTextA(dc, "DrawText wraps this paragraph at word boundaries inside its rectangle, "
                      "centres each line, and ends with an ellipsis if it runs out of room...", -1, &r,
                  DT_WORDBREAK | DT_CENTER);
        FrameRect(dc, &r, (HBRUSH)GetStockObject(GRAY_BRUSH));
        StretchBlt(dc, 10, y, 64, 64, mem, 0, 0, 8, 8, SRCCOPY);
        for (i = 0; i < 64 * 64; i++)
            grad[i] = ((i % 64) * 4) << 16 | ((i / 64) * 4) << 8 | 0x80;
        make_info(&bi, 64, 64, 32);
        StretchDIBits(dc, 90, y, 128, 64, 0, 0, 64, 64, grad, &bi, DIB_RGB_COLORS, SRCCOPY);
        DrawIconEx(dc, 240, y, LoadIcon(inst, MAKEINTRESOURCE(IDI_TEST)), 0, 0, 0, NULL, DI_NORMAL);
        DrawIcon(dc, 280, y, LoadIcon(NULL, IDI_APPLICATION));
        DrawIcon(dc, 320, y, LoadIcon(NULL, IDI_ERROR));
        DrawIcon(dc, 360, y, LoadIcon(NULL, IDI_WARNING));
        DrawIcon(dc, 400, y, LoadIcon(NULL, IDI_INFORMATION));
        DrawIcon(dc, 440, y, LoadIcon(NULL, IDI_QUESTION));
        SelectObject(mem, old);
        DeleteDC(mem);
        DeleteObject(bmp);
        EndPaint(hwnd, &ps);
        return 0;
    }
    if (m == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, m, w, l);
}

int main(int argc, char **argv)
{
    inst = GetModuleHandleA(NULL);
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1) {
        WNDCLASSA wc = { 0 };
        MSG msg;
        wc.lpfnWndProc = proc;
        wc.hInstance = inst;
        wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
        wc.lpszClassName = "w9gfx";
        RegisterClassA(&wc);
        ShowWindow(CreateWindowA("w9gfx", "G2: text and images", WS_OVERLAPPEDWINDOW, 60, 80, 600, 260, NULL, NULL, inst, NULL),
                   SW_SHOW);
        while (GetMessageA(&msg, NULL, 0, 0) > 0)
            DispatchMessageA(&msg);
        return 0;
    }
    dib_sections();
    dib_copies();
    blending();
    resources();
    fonts();
    return 0;
}
