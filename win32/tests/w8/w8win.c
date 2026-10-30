/* G1 test: windows, messages, painting. Prints the messages its window
 * procedures get (a few that depend on the desktop, the IME or the
 * compositor are left out) and what the window and drawing calls return,
 * then closes its own window.
 *
 *   w8win        the scripted run, compared with Windows' output
 *   w8win wait   draws a picture and stays open until closed from the
 *                title bar (WM_CLOSE -> WM_DESTROY -> WM_QUIT)
 */
#include <windows.h>
#include <stdio.h>

static int timers;
static int waiting;
static HWND child;

static const char *name(UINT m)
{
    switch (m) {
    case WM_CREATE: return "WM_CREATE";
    case WM_DESTROY: return "WM_DESTROY";
    case WM_MOVE: return "WM_MOVE";
    case WM_SIZE: return "WM_SIZE";
    case WM_ACTIVATE: return "WM_ACTIVATE";
    case WM_SETFOCUS: return "WM_SETFOCUS";
    case WM_KILLFOCUS: return "WM_KILLFOCUS";
    case WM_ENABLE: return "WM_ENABLE";
    case WM_SETTEXT: return "WM_SETTEXT";
    case WM_GETTEXT: return "WM_GETTEXT";
    case WM_GETTEXTLENGTH: return "WM_GETTEXTLENGTH";
    case WM_PAINT: return "WM_PAINT";
    case WM_CLOSE: return "WM_CLOSE";
    case WM_QUIT: return "WM_QUIT";
    case WM_ERASEBKGND: return "WM_ERASEBKGND";
    case WM_SHOWWINDOW: return "WM_SHOWWINDOW";
    case WM_ACTIVATEAPP: return "WM_ACTIVATEAPP";
    case WM_GETMINMAXINFO: return "WM_GETMINMAXINFO";
    case WM_WINDOWPOSCHANGING: return "WM_WINDOWPOSCHANGING";
    case WM_WINDOWPOSCHANGED: return "WM_WINDOWPOSCHANGED";
    case WM_NCCREATE: return "WM_NCCREATE";
    case WM_NCDESTROY: return "WM_NCDESTROY";
    case WM_NCCALCSIZE: return "WM_NCCALCSIZE";
    case WM_NCACTIVATE: return "WM_NCACTIVATE";
    case WM_PARENTNOTIFY: return "WM_PARENTNOTIFY";
    case WM_TIMER: return "WM_TIMER";
    case WM_STYLECHANGING: return "WM_STYLECHANGING";
    case WM_STYLECHANGED: return "WM_STYLECHANGED";
    case WM_CHILDACTIVATE: return "WM_CHILDACTIVATE";
    case WM_USER + 1: return "WM_USER+1";
    case WM_USER + 2: return "WM_USER+2";
    case WM_USER + 3: return "WM_USER+3";
    }
    return 0;
}

/* Messages whose presence depends on the desktop, the shell, the IME or
 * the compositor rather than on the program. */
static int noise(UINT m)
{
    switch (m) {
    case WM_GETICON: case WM_IME_SETCONTEXT: case WM_IME_NOTIFY: case WM_GETOBJECT:
    case WM_NCPAINT: case WM_NCHITTEST: case WM_SETCURSOR: case WM_MOUSEMOVE: case WM_NCMOUSEMOVE:
    case WM_NCMOUSELEAVE: case WM_MOUSEACTIVATE: case 0x031F /* WM_DWMNCRENDERINGCHANGED */:
    case 0x0090 /* WM_UAHDESTROYWINDOW */: case WM_SYNCPAINT: case WM_ACTIVATEAPP: case WM_NCACTIVATE:
    case WM_ACTIVATE: case WM_SETFOCUS: case WM_KILLFOCUS: case 0x0093: case 0x0094: case 0x0091: case 0x0092:
    case WM_DWMCOLORIZATIONCOLORCHANGED: case WM_ENTERIDLE: case WM_CANCELMODE: case WM_CAPTURECHANGED:
    case WM_MOVING: case WM_SIZING: case WM_ENTERSIZEMOVE: case WM_EXITSIZEMOVE: case WM_SYSCOMMAND:
    case WM_NCLBUTTONDOWN: case WM_LBUTTONDOWN: case WM_LBUTTONUP: case WM_CHAR: case WM_KEYDOWN: case WM_KEYUP:
    case 0x00AE /* WM_NCUAHDRAWCAPTION */: case 0x00AF /* WM_NCUAHDRAWFRAME */:
    case WM_SETTINGCHANGE: case WM_THEMECHANGED: case WM_DISPLAYCHANGE: case WM_DEVICECHANGE: case 0x0060:
        return 1;
    }
    return 0;
}

static void log_msg(const char *who, UINT m, WPARAM w, LPARAM l)
{
    const char *n = name(m);
    /* Registered messages (0xC000 up) come from the shell. */
    if (noise(m) || m >= 0xC000 || waiting)
        return;
    switch (m) {
    case WM_SIZE:
        printf("%s %s %d %d,%d\n", who, n, (int)w, (short)LOWORD(l), (short)HIWORD(l));
        return;
    case WM_MOVE:
        /* Where the desktop puts a top-level window is its business. */
        if (who[0] == 'c')
            printf("%s %s %d,%d\n", who, n, (short)LOWORD(l), (short)HIWORD(l));
        else
            printf("%s %s\n", who, n);
        return;
    case WM_SHOWWINDOW:
    case WM_TIMER:
    case WM_USER + 1:
    case WM_USER + 2:
        printf("%s %s %d %d\n", who, n, (int)w, (int)l);
        return;
    case WM_WINDOWPOSCHANGING:
    case WM_WINDOWPOSCHANGED: {
        WINDOWPOS *p = (WINDOWPOS *)l;
        if (who[0] == 'c')
            printf("%s %s %d,%d %dx%d", who, n, p->x, p->y, p->cx, p->cy);
        else
            printf("%s %s %dx%d", who, n, p->cx, p->cy);
        /* Without SWP_NOCLIENTSIZE, SWP_NOCLIENTMOVE, SWP_STATECHANGED and
         * the undocumented high bits. */
        printf(" flags %#x\n", p->flags & 0x07FF);
        return;
    }
    case WM_SETTEXT:
        printf("%s %s \"%s\"\n", who, n, (const char *)l);
        return;
    case WM_PARENTNOTIFY:
        printf("%s %s %d id %d\n", who, n, LOWORD(w), HIWORD(w));
        return;
    }
    if (n)
        printf("%s %s\n", who, n);
    else
        printf("%s %#x\n", who, m);
}

static void paint_picture(HWND hwnd, HDC dc)
{
    RECT rc;
    HBRUSH red = CreateSolidBrush(RGB(200, 40, 40));
    HPEN blue = CreatePen(PS_SOLID, 1, RGB(0, 0, 255));
    HGDIOBJ old_brush, old_pen;
    GetClientRect(hwnd, &rc);
    old_brush = SelectObject(dc, red);
    old_pen = SelectObject(dc, blue);
    Rectangle(dc, 20, 20, 120, 80);
    Ellipse(dc, 140, 20, 240, 80);
    MoveToEx(dc, 20, 100, NULL);
    LineTo(dc, 240, 140);
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    SetTextColor(dc, RGB(0, 0, 0));
    SetBkMode(dc, TRANSPARENT);
    TextOutA(dc, 20, 150, "Hello from a Windows program", 28);
    DeleteObject(red);
    DeleteObject(blue);
}

static LRESULT CALLBACK main_proc(HWND hwnd, UINT m, WPARAM w, LPARAM l)
{
    log_msg("main", m, w, l);
    switch (m) {
    case WM_CREATE: {
        CREATESTRUCTA *cs = (CREATESTRUCTA *)l;
        if (!waiting)
            printf("  create params %d, %dx%d\n", (int)(INT_PTR)cs->lpCreateParams, cs->cx, cs->cy);
        return 0;
    }
    case WM_PAINT: {
        PAINTSTRUCT ps;
        HDC dc = BeginPaint(hwnd, &ps);
        if (!waiting)
            printf("  paint rect %d,%d-%d,%d erase %d\n", (int)ps.rcPaint.left, (int)ps.rcPaint.top,
                   (int)ps.rcPaint.right, (int)ps.rcPaint.bottom, ps.fErase);
        paint_picture(hwnd, dc);
        EndPaint(hwnd, &ps);
        return 0;
    }
    case WM_TIMER:
        if (++timers == 3) {
            KillTimer(hwnd, 1);
            PostMessageA(hwnd, WM_USER + 1, 11, 12);
        }
        return 0;
    case WM_USER + 1:
        printf("  send returned %d\n", (int)SendMessageA(hwnd, WM_USER + 2, 21, 22));
        PostMessageA(hwnd, WM_CLOSE, 0, 0);
        return 0;
    case WM_USER + 2:
        return 42;
    case WM_DESTROY:
        PostQuitMessage(3);
        return 0;
    }
    return DefWindowProcA(hwnd, m, w, l);
}

static LRESULT CALLBACK child_proc(HWND hwnd, UINT m, WPARAM w, LPARAM l)
{
    log_msg("child", m, w, l);
    if (m == WM_PAINT) {
        PAINTSTRUCT ps;
        RECT rc;
        HDC dc = BeginPaint(hwnd, &ps);
        GetClientRect(hwnd, &rc);
        FillRect(dc, &rc, (HBRUSH)GetStockObject(BLACK_BRUSH));
        EndPaint(hwnd, &ps);
        return 0;
    }
    return DefWindowProcA(hwnd, m, w, l);
}

static void drawing_checks(void)
{
    /* Draw into a memory bitmap and read pixels back. */
    HDC screen = GetDC(NULL);
    HDC mem = CreateCompatibleDC(screen);
    HBITMAP bmp = CreateCompatibleBitmap(screen, 64, 32);
    HGDIOBJ old = SelectObject(mem, bmp);
    RECT r = { 0, 0, 64, 32 };
    HBRUSH green = CreateSolidBrush(RGB(0, 128, 0));
    SIZE sz;
    TEXTMETRICA tm;
    BITMAP bm;
    FillRect(mem, &r, (HBRUSH)GetStockObject(WHITE_BRUSH));
    SelectObject(mem, green);
    SelectObject(mem, GetStockObject(BLACK_PEN));
    Rectangle(mem, 4, 4, 20, 12);
    printf("rect pixels %06lx %06lx %06lx %06lx\n", GetPixel(mem, 4, 4), GetPixel(mem, 19, 11),
           GetPixel(mem, 10, 8), GetPixel(mem, 20, 12));
    MoveToEx(mem, 30, 2, NULL);
    LineTo(mem, 40, 2);
    printf("line pixels %06lx %06lx %06lx\n", GetPixel(mem, 30, 2), GetPixel(mem, 39, 2), GetPixel(mem, 40, 2));
    SetPixel(mem, 50, 20, RGB(1, 2, 3));
    printf("set pixel %06lx, outside %lx\n", GetPixel(mem, 50, 20), GetPixel(mem, 64, 0));
    BitBlt(mem, 40, 16, 10, 10, mem, 0, 0, SRCCOPY);
    printf("blt %06lx %06lx\n", GetPixel(mem, 44, 20), GetPixel(mem, 40, 16));
    BitBlt(mem, 0, 0, 2, 2, NULL, 0, 0, BLACKNESS);
    printf("blackness %06lx\n", GetPixel(mem, 1, 1));
    GetObjectA(bmp, sizeof bm, &bm);
    printf("bitmap %dx%d %d bpp %d planes\n", (int)bm.bmWidth, (int)bm.bmHeight, bm.bmBitsPixel, bm.bmPlanes);
    GetTextExtentPoint32A(mem, "", 0, &sz);
    printf("empty text extent %dx%d\n", (int)sz.cx, (int)sz.cy);
    GetTextMetricsA(mem, &tm);
    printf("text metrics consistent %d\n", tm.tmHeight == tm.tmAscent + tm.tmDescent);
    printf("old bitmap is a bitmap %d\n", GetObjectType(old) == OBJ_BITMAP);
    SelectObject(mem, old);
    DeleteObject(green);
    DeleteObject(bmp);
    DeleteDC(mem);
    ReleaseDC(NULL, screen);
    printf("stock delete %d\n", DeleteObject(GetStockObject(BLACK_BRUSH)));
}

int main(int argc, char **argv)
{
    WNDCLASSEXA wc = { sizeof wc };
    HWND hwnd;
    MSG msg;
    RECT rc;
    char text[64];
    int r;

    waiting = argc > 1;
    setvbuf(stdout, NULL, _IONBF, 0);
    wc.lpfnWndProc = main_proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.lpszClassName = "w8main";
    wc.cbWndExtra = 16;
    printf("register %d\n", RegisterClassExA(&wc) != 0);
    printf("register again %d error %lu\n", RegisterClassExA(&wc) != 0, GetLastError());
    wc.lpfnWndProc = child_proc;
    wc.lpszClassName = "w8child";
    wc.cbWndExtra = 0;
    RegisterClassExA(&wc);

    if (!waiting)
        drawing_checks();

    hwnd = CreateWindowExA(0, "w8main", "G1 test", WS_OVERLAPPEDWINDOW, 100, 100, 400, 300,
                           NULL, NULL, wc.hInstance, (void *)77);
    if (!hwnd) {
        printf("CreateWindowEx failed %lu\n", GetLastError());
        return 1;
    }
    if (!waiting) {
        GetClientRect(hwnd, &rc);
        printf("client %d,%d-%d,%d\n", (int)rc.left, (int)rc.top, (int)rc.right, (int)rc.bottom);
        GetWindowRect(hwnd, &rc);
        printf("window %dx%d\n", (int)(rc.right - rc.left), (int)(rc.bottom - rc.top));
        rc.left = 0; rc.top = 0; rc.right = 100; rc.bottom = 100;
        AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
        printf("adjust %d,%d-%d,%d\n", (int)rc.left, (int)rc.top, (int)rc.right, (int)rc.bottom);
        printf("visible %d, IsWindow %d, class extra %d\n", IsWindowVisible(hwnd), IsWindow(hwnd),
               (int)GetClassLongPtrA(hwnd, GCL_CBWNDEXTRA));
        SetWindowLongPtrA(hwnd, 8, 1234);
        SetWindowLongPtrA(hwnd, GWLP_USERDATA, 5678);
        printf("long %d %d style %#lx\n", (int)GetWindowLongPtrA(hwnd, 8), (int)GetWindowLongPtrA(hwnd, GWLP_USERDATA),
               (unsigned long)GetWindowLongA(hwnd, GWL_STYLE));
        GetWindowTextA(hwnd, text, sizeof text);
        printf("title \"%s\" length %d\n", text, GetWindowTextLengthA(hwnd));
        printf("ShowWindow\n");
    }
    ShowWindow(hwnd, SW_SHOW);
    if (!waiting)
        printf("UpdateWindow\n");
    UpdateWindow(hwnd);
    if (!waiting)
        printf("child\n");
    child = CreateWindowExA(0, "w8child", "", WS_CHILD | WS_VISIBLE, 10, 200, 100, 40, hwnd, (HMENU)7,
                            wc.hInstance, NULL);
    if (!waiting) {
        printf("parent %d id %d\n", GetParent(child) == hwnd, GetDlgCtrlID(child));
        GetWindowRect(child, &rc);
        MapWindowPoints(NULL, hwnd, (POINT *)&rc, 2);
        printf("child at %d,%d-%d,%d\n", (int)rc.left, (int)rc.top, (int)rc.right, (int)rc.bottom);
        printf("SetWindowText\n");
        SetWindowTextA(hwnd, "G1 test, renamed");
        printf("SetWindowPos\n");
        SetWindowPos(hwnd, NULL, 0, 0, 500, 350, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        GetClientRect(hwnd, &rc);
        printf("client now %dx%d\n", (int)rc.right, (int)rc.bottom);
        printf("loop\n");
        SetTimer(hwnd, 1, 30, NULL);
    }
    while ((r = GetMessageA(&msg, NULL, 0, 0)) > 0) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
    if (!waiting)
        printf("GetMessage %d, quit code %d, IsWindow %d\n", r, (int)msg.wParam, IsWindow(hwnd));
    return (int)msg.wParam;
}
