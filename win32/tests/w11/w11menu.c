/* G3b test: menus and accelerators. Menus built by hand and from MENU and
 * MENUEX resources, the system menu, a window's menu bar, accelerator
 * tables through TranslateAccelerator, a popup menu and a menu bar menu
 * driven from the keyboard by timers. Prints what the calls return and the
 * menu messages the window gets.
 *
 *   w11menu        the scripted run, compared with Windows' output
 *   w11menu wait   a window with a menu bar, open until File > Exit
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HWND win;
static int logging, waiting;
static HMENU bar;

static void print_menu(HMENU m, int depth)
{
    int i, n = GetMenuItemCount(m);
    char text[64];
    for (i = 0; i < n; i++) {
        UINT st = GetMenuState(m, i, MF_BYPOSITION);
        GetMenuStringA(m, i, text, sizeof text, MF_BYPOSITION);
        printf("%*s%d: id %d \"%s\" state %04x\n", depth * 2, "", i, (int)GetMenuItemID(m, i), text, st);
        if (st & MF_POPUP)
            print_menu(GetSubMenu(m, i), depth + 1);
    }
}

static void log_msg(UINT m, WPARAM w, LPARAM l)
{
    if (!logging)
        return;
    switch (m) {
    case WM_COMMAND:
        printf("  WM_COMMAND %d source %d\n", LOWORD(w), HIWORD(w));
        break;
    case WM_SYSCOMMAND:
        printf("  WM_SYSCOMMAND %04x key %d\n", (unsigned)(w & 0xFFF0), (int)l);
        break;
    case WM_INITMENU:
        printf("  WM_INITMENU bar %d\n", (HMENU)w == bar);
        break;
    case WM_INITMENUPOPUP:
        printf("  WM_INITMENUPOPUP position %d system %d\n", LOWORD(l), HIWORD(l));
        break;
    case WM_UNINITMENUPOPUP:
        printf("  WM_UNINITMENUPOPUP\n");
        break;
    case WM_MENUSELECT:
        printf("  WM_MENUSELECT item %d flags %04x menu %d\n", LOWORD(w), HIWORD(w), l != 0);
        break;
    case WM_ENTERMENULOOP:
        printf("  WM_ENTERMENULOOP popup %d\n", (int)w);
        break;
    case WM_EXITMENULOOP:
        printf("  WM_EXITMENULOOP popup %d\n", (int)w);
        break;
    case WM_SIZE:
        printf("  WM_SIZE %d,%d\n", LOWORD(l), HIWORD(l));
        break;
    }
}

static LRESULT CALLBACK proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    log_msg(m, w, l);
    if (waiting && m == WM_COMMAND && LOWORD(w) == 202)
        DestroyWindow(h);
    if (m == WM_DESTROY)
        PostQuitMessage(0);
    return DefWindowProcA(h, m, w, l);
}

static void pump(void)
{
    MSG msg;
    while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }
}

/* Keys for the menu loop, from a timer once it has started. */
static int script[8], script_at;
static void CALLBACK keys(HWND h, UINT m, UINT_PTR id, DWORD t)
{
    int vk = script[script_at++];
    if (!script[script_at])
        KillTimer(NULL, id);
    PostMessageA(win, WM_KEYDOWN, vk, 1);
    PostMessageA(win, WM_KEYUP, vk, 0xC0000001);
}

static void start_keys(int a, int b, int c)
{
    script[0] = a;
    script[1] = b;
    script[2] = c;
    script[3] = 0;
    script_at = 0;
    SetTimer(NULL, 0, 60, keys);
}

static void accel_try(HACCEL acc, UINT m, WPARAM vk, int ctrl)
{
    MSG msg = { win, m, vk, 1 };
    BYTE ks[256];
    int r;
    GetKeyboardState(ks);
    ks[VK_CONTROL] = ctrl ? 0x80 : 0;
    SetKeyboardState(ks);
    r = TranslateAcceleratorA(win, acc, &msg);
    printf("TranslateAccelerator %s %d ctrl %d -> %d\n", m == WM_CHAR ? "char" : "key", (int)vk, ctrl, r);
}

int main(int argc, char **argv)
{
    HINSTANCE inst = GetModuleHandleA(NULL);
    WNDCLASSA wc = { 0 };
    HMENU m, pop, sys;
    MENUITEMINFOA mi;
    ACCEL accel[3] = { { FVIRTKEY | FCONTROL, 'S', 100 }, { 0, 'a', 101 }, { FVIRTKEY, VK_F5, 102 } };
    ACCEL out[8];
    HACCEL acc;
    RECT r;
    char text[64];
    int i, n;

    setvbuf(stdout, NULL, _IONBF, 0);
    wc.lpfnWndProc = proc;
    wc.hInstance = inst;
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.lpszClassName = "w11menu";
    RegisterClassA(&wc);
    if (argc > 1) {
        MSG msg;
        waiting = 1;
        win = CreateWindowA("w11menu", "G3b: menus", WS_OVERLAPPEDWINDOW, 80, 80, 420, 240, NULL, LoadMenuA(inst, MAKEINTRESOURCE(101)), inst, NULL);
        acc = LoadAcceleratorsA(inst, MAKEINTRESOURCE(103));
        ShowWindow(win, SW_SHOW);
        while (GetMessageA(&msg, NULL, 0, 0) > 0)
            if (!TranslateAcceleratorA(win, acc, &msg)) {
                TranslateMessage(&msg);
                DispatchMessageA(&msg);
            }
        return 0;
    }

    printf("built menus\n");
    m = CreateMenu();
    pop = CreatePopupMenu();
    AppendMenuA(pop, MF_STRING, 10, "&One");
    AppendMenuA(pop, MF_STRING | MF_CHECKED, 11, "&Two");
    AppendMenuA(pop, MF_SEPARATOR, 0, NULL);
    AppendMenuA(pop, MF_STRING | MF_GRAYED, 12, "T&hree");
    AppendMenuA(m, MF_POPUP, (UINT_PTR)pop, "&Menu");
    AppendMenuA(m, MF_STRING, 20, "&Item");
    print_menu(m, 0);
    printf("CheckMenuItem was %u, EnableMenuItem was %u\n", CheckMenuItem(pop, 10, MF_CHECKED), EnableMenuItem(pop, 12, MF_ENABLED));
    printf("CheckMenuRadioItem %d\n", CheckMenuRadioItem(pop, 10, 11, 11, MF_BYCOMMAND));
    InsertMenuA(pop, 0, MF_BYPOSITION | MF_STRING, 9, "&Zero");
    ModifyMenuA(pop, 20, MF_BYCOMMAND | MF_STRING, 21, "x");
    ModifyMenuA(m, 20, MF_BYCOMMAND | MF_STRING, 21, "&Renamed");
    memset(&mi, 0, sizeof mi);
    mi.cbSize = sizeof mi;
    mi.fMask = MIIM_ID | MIIM_STATE | MIIM_STRING | MIIM_FTYPE;
    mi.dwTypeData = text;
    mi.cch = sizeof text;
    GetMenuItemInfoA(pop, 2, TRUE, &mi);
    printf("item info id %u state %04x type %04x \"%s\"\n", mi.wID, mi.fState, mi.fType, text);
    mi.fMask = MIIM_STATE;
    mi.fState = MFS_DEFAULT;
    SetMenuItemInfoA(pop, 9, FALSE, &mi);
    printf("default item %d\n", (int)GetMenuDefaultItem(pop, FALSE, 0));
    print_menu(m, 0);
    printf("DeleteMenu %d, RemoveMenu %d, count %d\n", DeleteMenu(pop, 12, MF_BYCOMMAND), RemoveMenu(pop, 0, MF_BYPOSITION), GetMenuItemCount(pop));
    printf("missing item state %d, IsMenu %d\n", (int)GetMenuState(pop, 99, MF_BYCOMMAND), IsMenu(pop));
    DestroyMenu(m);
    printf("after DestroyMenu: IsMenu %d %d\n", IsMenu(m), IsMenu(pop));

    printf("MENU resource\n");
    bar = LoadMenuA(inst, MAKEINTRESOURCE(101));
    print_menu(bar, 0);
    printf("MENUEX resource\n");
    m = LoadMenuA(inst, MAKEINTRESOURCE(102));
    print_menu(m, 0);
    mi.fMask = MIIM_FTYPE | MIIM_STATE | MIIM_ID;
    GetMenuItemInfoA(GetSubMenu(m, 0), 2, TRUE, &mi);
    printf("radio item type %04x state %04x id %u\n", mi.fType, mi.fState, mi.wID);
    DestroyMenu(m);

    printf("window with a menu bar\n");
    win = CreateWindowA("w11menu", "G3b", WS_OVERLAPPEDWINDOW, 100, 100, 400, 300, NULL, bar, inst, NULL);
    printf("GetMenu is bar %d\n", GetMenu(win) == bar);
    GetClientRect(win, &r);
    printf("client %ldx%ld\n", r.right, r.bottom);
    logging = 1;
    SetMenu(win, NULL);
    GetClientRect(win, &r);
    printf("without menu: client %ldx%ld, GetMenu %d\n", r.right, r.bottom, GetMenu(win) != NULL);
    SetMenu(win, bar);
    GetClientRect(win, &r);
    printf("with menu: client %ldx%ld\n", r.right, r.bottom);
    logging = 0;
    sys = GetSystemMenu(win, FALSE);
    n = GetMenuItemCount(sys);
    printf("system menu %d items:", n);
    for (i = 0; i < n; i++)
        printf(" %04x", GetMenuItemID(sys, i));
    printf("\n");
    GetMenuStringA(sys, SC_CLOSE, text, sizeof text, MF_BYCOMMAND);
    printf("close \"%s\"\n", text);

    printf("accelerators\n");
    acc = CreateAcceleratorTableA(accel, 3);
    n = CopyAcceleratorTableA(acc, NULL, 0);
    CopyAcceleratorTableA(acc, out, 8);
    printf("count %d, second %d '%c' %d\n", n, out[1].fVirt, out[1].key, out[1].cmd);
    logging = 1;
    accel_try(acc, WM_KEYDOWN, 'S', 0);
    accel_try(acc, WM_KEYDOWN, 'S', 1);
    accel_try(acc, WM_KEYDOWN, VK_F5, 0);
    accel_try(acc, WM_CHAR, 'a', 0);
    accel_try(acc, WM_CHAR, 'b', 0);
    logging = 0;
    DestroyAcceleratorTable(acc);
    acc = LoadAcceleratorsA(inst, MAKEINTRESOURCE(103));
    n = CopyAcceleratorTableA(acc, out, 8);
    printf("resource table %d:", n);
    for (i = 0; i < n; i++)
        printf(" (%02x %d %d)", out[i].fVirt, out[i].key, out[i].cmd);
    printf("\n");
    EnableMenuItem(bar, 200, MF_GRAYED);
    logging = 1;
    accel_try(acc, WM_KEYDOWN, 'N', 1);
    EnableMenuItem(bar, 200, MF_ENABLED);
    accel_try(acc, WM_KEYDOWN, 'N', 1);
    accel_try(acc, WM_KEYDOWN, VK_F1, 0);
    accel_try(acc, WM_KEYDOWN, 'N', 0);
    logging = 0;
    {
        BYTE ks[256];
        GetKeyboardState(ks);
        ks[VK_CONTROL] = 0;
        SetKeyboardState(ks);
    }

    printf("popup menu\n");
    ShowWindow(win, SW_SHOW);
    SetForegroundWindow(win);
    pump();
    pop = CreatePopupMenu();
    AppendMenuA(pop, MF_STRING, 40, "&First");
    AppendMenuA(pop, MF_STRING, 41, "&Second");
    logging = 1;
    start_keys(VK_DOWN, VK_DOWN, VK_RETURN);
    i = TrackPopupMenu(pop, TPM_RETURNCMD | TPM_NONOTIFY, 150, 150, 0, win, NULL);
    printf("TrackPopupMenu returned %d\n", i);
    start_keys(VK_DOWN, VK_RETURN, 0);
    i = TrackPopupMenu(pop, 0, 150, 150, 0, win, NULL);
    printf("TrackPopupMenu returned %d\n", i);
    pump();
    start_keys(VK_ESCAPE, 0, 0);
    i = TrackPopupMenu(pop, TPM_RETURNCMD, 150, 150, 0, win, NULL);
    printf("cancelled: returned %d\n", i);
    pump();

    printf("menu bar from the keyboard\n");
    start_keys(VK_DOWN, VK_RETURN, 0);
    PostMessageA(win, WM_SYSCHAR, 'f', 0x20000001);
    pump();
    for (i = 0; i < 20; i++) {
        Sleep(50);
        pump();
    }
    logging = 0;
    DestroyWindow(win);
    return 0;
}
