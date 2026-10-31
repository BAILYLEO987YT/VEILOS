/* W12 (G4a): what real programs (Notepad) lean on. Registered messages and
 * clipboard formats, window properties, Char* and wsprintf, moveable
 * memory, file mapping, MulDiv, FindNLSString, IsTextUnicode, the status
 * bar, image lists, the edit control's buffer handle and file lines,
 * WinEvent hooks, the Find dialog, GetFileTitle, CommandLineToArgvW and
 * FindMimeFromData. (The clipboard is left out: running this on Windows
 * would replace the user's clipboard.) */
#include <windows.h>
#include <commctrl.h>
#include <commdlg.h>
#include <shellapi.h>
#include <urlmon.h>
#include <stdio.h>
#include <wchar.h>

static void wprint(const char *label, const WCHAR *s)
{
    char buf[512];
    WideCharToMultiByte(CP_UTF8, 0, s, -1, buf, sizeof buf, NULL, NULL);
    printf("%s: \"%s\"\n", label, buf);
}

static LRESULT CALLBACK plain_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    return DefWindowProcW(h, m, w, l);
}

/* -----| registered names and properties |----- */

static void names(HWND hwnd)
{
    UINT a = RegisterWindowMessageW(L"commdlg_FindReplace");
    UINT b = RegisterWindowMessageW(L"COMMDLG_FINDREPLACE");
    UINT c = RegisterWindowMessageW(L"VeilTest_Other");
    printf("message: in range %d, same name same id %d, other differs %d\n", a >= 0xC000 && a <= 0xFFFF, a == b, a != c);
    UINT f = RegisterClipboardFormatW(L"VeilTest Format");
    UINT g = RegisterClipboardFormatW(L"VeilTest Format");
    WCHAR name[64] = {0};
    int n = GetClipboardFormatNameW(f, name, 64);
    printf("format: in range %d, same %d, name length %d\n", f >= 0xC000, f == g, n);
    wprint("format name", name);
    n = GetClipboardFormatNameW(CF_TEXT, name, 64);
    printf("predefined format name: %d\n", n);

    BOOL set = SetPropW(hwnd, L"Answer", (HANDLE)42);
    HANDLE got = GetPropW(hwnd, L"answer");
    HANDLE missing = GetPropW(hwnd, L"Question");
    printf("props: set %d, get %d, missing %d\n", set, (int)(INT_PTR)got, (int)(INT_PTR)missing);
    ATOM atom = GlobalAddAtomW(L"VeilTestAtom");
    SetPropW(hwnd, (LPCWSTR)MAKEINTATOM(atom), (HANDLE)7);
    got = GetPropW(hwnd, L"VeilTestAtom");
    printf("props: by atom then name %d\n", (int)(INT_PTR)got);
    HANDLE removed = RemovePropW(hwnd, L"Answer");
    got = GetPropW(hwnd, L"Answer");
    printf("props: removed %d, after %d\n", (int)(INT_PTR)removed, (int)(INT_PTR)got);
    RemovePropW(hwnd, (LPCWSTR)MAKEINTATOM(atom));
    GlobalDeleteAtom(atom);
}

/* -----| characters and formatting |----- */

static void text(void)
{
    WCHAR s[] = L"Hello, World 123";
    CharUpperW(s);
    wprint("CharUpperW", s);
    DWORD n = CharLowerBuffW(s, 5);
    printf("CharLowerBuffW: %lu\n", n);
    wprint("after", s);
    WCHAR one = (WCHAR)(ULONG_PTR)CharUpperW((LPWSTR)(ULONG_PTR)L'q');
    printf("CharUpperW single: %c\n", (char)one);
    const WCHAR *p = L"ab";
    const WCHAR *q = CharNextW(p);
    const WCHAR *r = CharNextW(q + 1);
    const WCHAR *back = CharPrevW(p, p);
    printf("CharNextW: %d %d, CharPrevW at start %d\n", (int)(q - p), (int)(r - p), (int)(back - p));
    printf("IsCharAlphaW a %d 1 %d, IsCharAlphaNumericW 1 %d, IsCharUpperW A %d a %d, IsCharLowerW a %d\n",
           IsCharAlphaW(L'a'), IsCharAlphaW(L'1'), IsCharAlphaNumericW(L'1'), IsCharUpperW(L'A'), IsCharUpperW(L'a'), IsCharLowerW(L'a'));

    WCHAR out[256];
    int len = wsprintfW(out, L"[%d] [%5d] [%-5d] [%05d] [%x] [%X] [%08lX] [%u] [%s] [%c] [%%] [%ld]",
                        -42, 7, 7, 42, 255, 255, 0xBEEFUL, 4000000000U, L"str", L'Z', -100000L);
    printf("wsprintfW: %d\n", len);
    wprint("wsprintfW", out);
    char outa[256];
    len = wsprintfA(outa, "%s|%-3s|%3s|%.2s|%i|%#x|%hs|%ls", "abc", "a", "b", "xyz", 12, 31, "narrow", L"wide");
    printf("wsprintfA: %d \"%s\"\n", len, outa);
}

/* -----| memory |----- */

static void memory(void)
{
    HLOCAL h = LocalAlloc(LMEM_MOVEABLE | LMEM_ZEROINIT, 16);
    char *p = (char *)LocalLock(h);
    printf("moveable: handle is not the data %d, data zeroed %d, flags %u\n", (void *)h != (void *)p, p[0] == 0 && p[15] == 0, LocalFlags(h));
    strcpy(p, "kept");
    SetLastError(1234);
    BOOL still = LocalUnlock(h);
    DWORD err = GetLastError();
    printf("unlock: %d, error %lu, flags %u\n", still, err, LocalFlags(h));
    HLOCAL h2 = LocalReAlloc(h, 5000, LMEM_MOVEABLE);
    SIZE_T size = LocalSize(h);
    p = (char *)LocalLock(h);
    HLOCAL back = LocalHandle(p);
    printf("realloc: same handle %d, size %zu, contents \"%s\", LocalHandle %d\n", h2 == h, size, p, back == h);
    LocalUnlock(h);
    HLOCAL freed = LocalFree(h);
    printf("free: %d\n", freed == NULL);

    HLOCAL f = LocalAlloc(LMEM_FIXED, 32);
    void *fp = LocalLock(f);
    SetLastError(0);
    still = LocalUnlock(f);
    err = GetLastError();
    printf("fixed: lock is the handle %d, unlock %d, error %lu, LocalHandle %d\n", fp == (void *)f, still, err, LocalHandle(fp) == f);
    LocalFree(f);

    HGLOBAL g = GlobalAlloc(GMEM_MOVEABLE, 0);
    printf("zero-size moveable: flags %#x, lock %d\n", GlobalFlags(g), GlobalLock(g) == NULL);
    g = GlobalReAlloc(g, 8, GMEM_MOVEABLE);
    printf("revived: %d, size %zu\n", GlobalLock(g) != NULL, GlobalSize(g));
    GlobalUnlock(g);
    GlobalFree(g);

    printf("MulDiv: %d %d %d %d %d %d\n", MulDiv(10, 3, 2), MulDiv(-5, 3, 2), MulDiv(5, 3, 2), MulDiv(7, 1, 2), MulDiv(1, 1, 0), MulDiv(-7, -1, 2));
}

static void mapping(void)
{
    char path[MAX_PATH];
    GetTempPathA(MAX_PATH, path);
    strcat(path, "w12map.txt");
    HANDLE f = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD done;
    WriteFile(f, "hello mapping", 13, &done, NULL);
    HANDLE m = CreateFileMappingA(f, NULL, PAGE_READWRITE, 0, 0, NULL);
    char *v = (char *)MapViewOfFile(m, FILE_MAP_ALL_ACCESS, 0, 0, 0);
    printf("mapping: %d %d, view \"%.13s\"\n", m != NULL, v != NULL, v);
    v[0] = 'J';
    FlushViewOfFile(v, 0);
    UnmapViewOfFile(v);
    CloseHandle(m);
    char buf[32] = {0};
    SetFilePointer(f, 0, NULL, FILE_BEGIN);
    ReadFile(f, buf, 13, &done, NULL);
    printf("written back: \"%s\"\n", buf);
    CloseHandle(f);
    DeleteFileA(path);
}

/* -----| text helpers |----- */

static void nls(void)
{
    int found = 0;
    int at = FindNLSString(LOCALE_USER_DEFAULT, FIND_FROMSTART, L"Hello World", -1, L"World", -1, &found);
    printf("FindNLSString: %d len %d\n", at, found);
    at = FindNLSString(LOCALE_USER_DEFAULT, FIND_FROMSTART | LINGUISTIC_IGNORECASE, L"Hello World", -1, L"world", -1, &found);
    printf("ignoring case: %d len %d\n", at, found);
    at = FindNLSString(LOCALE_USER_DEFAULT, FIND_FROMSTART, L"Hello World", -1, L"world", -1, &found);
    printf("with case: %d\n", at);
    at = FindNLSString(LOCALE_USER_DEFAULT, FIND_FROMEND, L"abcabc", -1, L"bc", -1, &found);
    printf("from end: %d\n", at);

    BYTE bom[] = {0xFF, 0xFE, 'a', 0, 'b', 0};
    INT t = IS_TEXT_UNICODE_SIGNATURE;
    BOOL u = IsTextUnicode(bom, sizeof bom, &t);
    printf("IsTextUnicode signature: %d %#x\n", u, t);
    BYTE rev[] = {0xFE, 0xFF, 0, 'a'};
    t = IS_TEXT_UNICODE_REVERSE_SIGNATURE;
    u = IsTextUnicode(rev, sizeof rev, &t);
    printf("reverse signature: %d %#x\n", u, t);

    int argc = 0;
    LPWSTR *argv = CommandLineToArgvW(L"prog.exe one \"two words\" three\\\\\"q\\\" \"\"", &argc);
    printf("CommandLineToArgvW: %d\n", argc);
    for (int i = 0; i < argc; i++) {
        char label[16];
        sprintf(label, "  argv[%d]", i);
        wprint(label, argv[i]);
    }
    LocalFree(argv);

    WCHAR title[64];
    short r = GetFileTitleW(L"C:\\some dir\\file.txt", title, 64);
    printf("GetFileTitleW: %d\n", r);
    wprint("title", title);
    r = GetFileTitleW(L"C:\\some dir\\file.txt", title, 4);
    printf("too small: %d\n", r);
    r = GetFileTitleW(L"C:\\dir\\", title, 64);
    printf("folder: %d\n", r);

    LPWSTR mime = NULL;
    static char sample[] = "hello world";
    HRESULT hr = FindMimeFromData(NULL, L"notes.txt", sample, 11, NULL, 0, &mime, 0);
    printf("FindMimeFromData txt: %#lx\n", hr);
    if (mime) {
        wprint("  mime", mime);
        CoTaskMemFree(mime);
        mime = NULL;
    }
}

/* -----| controls |----- */

static void status(HWND hwnd)
{
    HWND sb = CreateStatusWindowW(WS_CHILD | WS_VISIBLE, L"Ready", hwnd, 100);
    printf("status bar: %d\n", sb != NULL);
    int parts[3] = {100, 250, -1};
    LRESULT ok = SendMessageW(sb, SB_SETPARTS, 3, (LPARAM)parts);
    int got[4] = {0};
    LRESULT n = SendMessageW(sb, SB_GETPARTS, 4, (LPARAM)got);
    printf("parts: set %d, count %d: %d %d %d\n", (int)ok, (int)n, got[0], got[1], got[2]);
    SendMessageW(sb, SB_SETTEXTW, 1, (LPARAM)L"Ln 1, Col 1");
    SendMessageW(sb, SB_SETTEXTW, 2 | SBT_NOBORDERS, (LPARAM)L"UTF-8");
    LRESULT len = SendMessageW(sb, SB_GETTEXTLENGTHW, 1, 0);
    WCHAR buf[64] = {0};
    LRESULT r = SendMessageW(sb, SB_GETTEXTW, 1, (LPARAM)buf);
    printf("part 1: length %d, get %#x\n", (int)LOWORD(len), (unsigned)r);
    wprint("part 1", buf);
    r = SendMessageW(sb, SB_GETTEXTLENGTHW, 2, 0);
    printf("part 2 type: %#x\n", (unsigned)HIWORD(r));
    RECT pr;
    SendMessageW(sb, SB_GETRECT, 1, (LPARAM)&pr);
    printf("part 1 spans %d..%d\n", pr.left, pr.right);
    int borders[3];
    SendMessageW(sb, SB_GETBORDERS, 0, (LPARAM)borders);
    printf("borders: %d %d %d\n", borders[0], borders[1], borders[2]);
    DestroyWindow(sb);
}

static void images(void)
{
    HIMAGELIST il = ImageList_Create(16, 16, ILC_COLOR32, 4, 4);
    printf("image list: %d, count %d\n", il != NULL, ImageList_GetImageCount(il));
    HDC screen = GetDC(NULL);
    HBITMAP bm = CreateCompatibleBitmap(screen, 48, 16);
    ReleaseDC(NULL, screen);
    int first = ImageList_Add(il, bm, NULL);
    int second = ImageList_Add(il, bm, NULL);
    printf("added at %d then %d, count %d\n", first, second, ImageList_GetImageCount(il));
    int cx = 0, cy = 0;
    ImageList_GetIconSize(il, &cx, &cy);
    printf("icon size %dx%d\n", cx, cy);
    BOOL rm = ImageList_Remove(il, 1);
    printf("remove: %d, count %d\n", rm, ImageList_GetImageCount(il));
    ImageList_SetImageCount(il, 9);
    printf("set count: %d\n", ImageList_GetImageCount(il));
    COLORREF old = ImageList_SetBkColor(il, RGB(1, 2, 3));
    printf("bk color: %#lx then %#lx\n", old, ImageList_GetBkColor(il));
    ImageList_Remove(il, -1);
    printf("cleared: %d\n", ImageList_GetImageCount(il));
    DeleteObject(bm);
    ImageList_Destroy(il);
}

static void edit(HWND hwnd)
{
    HWND e = CreateWindowExW(0, L"EDIT", L"abc", WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOHSCROLL | ES_AUTOVSCROLL,
                             0, 0, 300, 200, hwnd, (HMENU)1, NULL, NULL);
    /* Notepad's way of loading a file: grow the edit's own block, fill it,
     * hand it back. */
    HLOCAL h = (HLOCAL)SendMessageW(e, EM_GETHANDLE, 0, 0);
    HLOCAL grown = LocalReAlloc(h, 200, LMEM_MOVEABLE);
    WCHAR *p = (WCHAR *)LocalLock(h);
    wcscpy(p, L"first line\r\nsecond\r\n\r\nlast");
    LocalUnlock(h);
    SendMessageW(e, EM_SETHANDLE, (WPARAM)h, 0);
    WCHAR buf[128];
    int n = GetWindowTextW(e, buf, 128);
    printf("EM_SETHANDLE: grew in place %d, text length %d\n", grown == h, n);
    HLOCAL again = (HLOCAL)SendMessageW(e, EM_GETHANDLE, 0, 0);
    printf("EM_GETHANDLE again: same %d\n", again == h);
    printf("lines %d, file lines %d\n", (int)SendMessageW(e, EM_GETLINECOUNT, 0, 0), (int)SendMessageW(e, EM_GETFILELINECOUNT, 0, 0));
    SendMessageW(e, EM_SETSEL, 15, 15);
    printf("caret %d, file line of caret %d, its start %d, its length %d\n", (int)SendMessageW(e, EM_GETCARETINDEX, 0, 0),
           (int)SendMessageW(e, EM_FILELINEFROMCHAR, (WPARAM)-1, 0), (int)SendMessageW(e, EM_FILELINEINDEX, (WPARAM)-1, 0),
           (int)SendMessageW(e, EM_FILELINELENGTH, (WPARAM)-1, 0));
    printf("line 3 starts at %d\n", (int)SendMessageW(e, EM_FILELINEINDEX, 3, 0));
    WCHAR line[32];
    *(WORD *)line = 31;
    LRESULT got = SendMessageW(e, EM_GETFILELINE, 1, (LPARAM)line);
    line[got] = 0;
    printf("EM_GETFILELINE 1: %d\n", (int)got);
    wprint("  line", line);
    SendMessageW(e, EM_SETCARETINDEX, 22, 0);
    DWORD s0 = 0, s1 = 0;
    SendMessageW(e, EM_GETSEL, (WPARAM)&s0, (LPARAM)&s1);
    printf("EM_SETCARETINDEX 22: selection %lu..%lu\n", s0, s1);
    printf("end of line: %d\n", (int)SendMessageW(e, EM_GETENDOFLINE, 0, 0));
    DestroyWindow(e);
}

/* -----| WinEvent hooks |----- */

static int events;
static LONG last_object, last_child;

static void CALLBACK on_event(HWINEVENTHOOK hook, DWORD event, HWND hwnd, LONG object, LONG child, DWORD thread, DWORD time)
{
    if (event == EVENT_OBJECT_NAMECHANGE && child == 5) {
        events++;
        last_object = object;
        last_child = child;
    }
}

static void pump(void)
{
    MSG m;
    while (PeekMessageW(&m, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&m);
        DispatchMessageW(&m);
    }
}

static void winevents(HWND hwnd)
{
    HWINEVENTHOOK hook = SetWinEventHook(EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE, NULL, on_event,
                                         GetCurrentProcessId(), 0, WINEVENT_OUTOFCONTEXT);
    printf("hook: %d\n", hook != NULL);
    NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, hwnd, OBJID_CLIENT, 5);
    int before = events;
    pump();
    printf("events: before the message loop %d, after %d (object %ld, child %ld)\n", before, events, last_object, last_child);
    BOOL un = UnhookWinEvent(hook);
    NotifyWinEvent(EVENT_OBJECT_NAMECHANGE, hwnd, OBJID_CLIENT, 5);
    pump();
    printf("unhooked %d, events %d\n", un, events);
}

/* -----| Find |----- */

static UINT find_msg;
static DWORD find_flags[4];
static WCHAR find_text[4][32];
static int finds;

static LRESULT CALLBACK owner_proc(HWND h, UINT m, WPARAM w, LPARAM l)
{
    if (m == find_msg && finds < 4) {
        FINDREPLACEW *fr = (FINDREPLACEW *)l;
        find_flags[finds] = fr->Flags;
        wcscpy(find_text[finds], fr->lpstrFindWhat);
        finds++;
        return 0;
    }
    return DefWindowProcW(h, m, w, l);
}

static void find(HWND owner)
{
    find_msg = RegisterWindowMessageW(FINDMSGSTRINGW);
    static WCHAR what[32] = L"start";
    static FINDREPLACEW fr;
    fr.lStructSize = sizeof fr;
    fr.hwndOwner = owner;
    fr.Flags = FR_DOWN | FR_HIDEWHOLEWORD;
    fr.lpstrFindWhat = what;
    fr.wFindWhatLen = 32;
    HWND dlg = FindTextW(&fr);
    printf("FindTextW: %d, visible %d\n", dlg != NULL, IsWindowVisible(dlg));
    WCHAR shown[32] = {0};
    GetDlgItemTextW(dlg, 1152, shown, 32);
    wprint("find box", shown);
    SetDlgItemTextW(dlg, 1152, L"needle");
    CheckDlgButton(dlg, 1041, BST_CHECKED);
    SendMessageW(dlg, WM_COMMAND, IDOK, (LPARAM)GetDlgItem(dlg, IDOK));
    SendMessageW(dlg, WM_COMMAND, IDCANCEL, (LPARAM)GetDlgItem(dlg, IDCANCEL));
    pump();
    printf("reports: %d\n", finds);
    for (int i = 0; i < finds; i++) {
        printf("  flags %#lx", find_flags[i]);
        wprint("", find_text[i]);
    }
    printf("dialog gone: %d\n", !IsWindow(dlg));
    printf("CommDlgExtendedError: %lu\n", CommDlgExtendedError());
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    CoInitialize(NULL);
    INITCOMMONCONTROLSEX icc = {sizeof icc, ICC_BAR_CLASSES};
    printf("InitCommonControlsEx: %d\n", InitCommonControlsEx(&icc));
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = plain_proc;
    wc.hInstance = GetModuleHandleW(NULL);
    wc.lpszClassName = L"W12Plain";
    RegisterClassW(&wc);
    wc.lpfnWndProc = owner_proc;
    wc.lpszClassName = L"W12Owner";
    RegisterClassW(&wc);
    HWND hwnd = CreateWindowExW(0, L"W12Plain", L"w12", WS_OVERLAPPEDWINDOW, 10, 10, 500, 300, NULL, NULL, wc.hInstance, NULL);
    HWND owner = CreateWindowExW(0, L"W12Owner", L"w12 owner", WS_OVERLAPPEDWINDOW, 20, 20, 400, 300, NULL, NULL, wc.hInstance, NULL);
    pump();

    names(hwnd);
    text();
    memory();
    mapping();
    nls();
    status(hwnd);
    images();
    edit(hwnd);
    winevents(hwnd);
    find(owner);

    DestroyWindow(owner);
    DestroyWindow(hwnd);
    return 0;
}
