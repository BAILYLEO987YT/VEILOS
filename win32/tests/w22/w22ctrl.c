/* W22 (S5b): Ctrl+C and Ctrl+Break. Control handlers in their order, on a
 * thread of their own; ignoring Ctrl+C; Ctrl+C typed with processed input on
 * and off; window-size events. Runs in a console of its own (AllocConsole), so
 * the events reach nothing else; prints to stdout, which the caller redirects
 * to a file. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HANDLE in, out, called;
static DWORD main_tid;
static char log_[512];
static int log_len;
static BOOL second_says = TRUE;

static void note(const char *who, DWORD ev)
{
    log_len += sprintf(log_ + log_len, " %s(%lu%s)", who, ev, GetCurrentThreadId() == main_tid ? ",main" : "");
}

static BOOL WINAPI first(DWORD ev)
{
    note("first", ev);
    SetEvent(called);
    return TRUE;
}

static BOOL WINAPI second(DWORD ev)
{
    note("second", ev);
    if (second_says)
        SetEvent(called);
    return second_says;
}

/* Raise `ev`, wait for a handler that says it's handled, report who ran. */
static void raise_(const char *label, DWORD ev)
{
    log_len = 0;
    log_[0] = 0;
    ResetEvent(called);
    BOOL ok = GenerateConsoleCtrlEvent(ev, 0);
    DWORD w = WaitForSingleObject(called, 2000);
    Sleep(100);
    printf("%s: generate %d, %s, ran:%s\n", label, ok, w == WAIT_OBJECT_0 ? "handled" : "nothing handled it", log_);
}

static void key(INPUT_RECORD *r, BOOL down, WORD vk, WORD scan, wchar_t ch, DWORD state)
{
    memset(r, 0, sizeof *r);
    r->EventType = KEY_EVENT;
    r->Event.KeyEvent.bKeyDown = down;
    r->Event.KeyEvent.wRepeatCount = 1;
    r->Event.KeyEvent.wVirtualKeyCode = vk;
    r->Event.KeyEvent.wVirtualScanCode = scan;
    r->Event.KeyEvent.uChar.UnicodeChar = ch;
    r->Event.KeyEvent.dwControlKeyState = state;
}

static void show_input(const char *label)
{
    DWORD n = 0;
    GetNumberOfConsoleInputEvents(in, &n);
    INPUT_RECORD r[16];
    DWORD got = 0;
    if (n)
        ReadConsoleInputW(in, r, n > 16 ? 16 : n, &got);
    DWORD counted = 0;
    for (DWORD i = 0; i < got; i++)
        counted += r[i].EventType != FOCUS_EVENT && r[i].EventType != MENU_EVENT;
    printf("%s: %lu waiting\n", label, counted);
    for (DWORD i = 0; i < got; i++) {
        if (r[i].EventType == KEY_EVENT) {
            KEY_EVENT_RECORD *k = &r[i].Event.KeyEvent;
            printf("  key %s vk %02x char %04x state %lx\n", k->bKeyDown ? "down" : "up", k->wVirtualKeyCode, k->uChar.UnicodeChar, k->dwControlKeyState);
        } else if (r[i].EventType == WINDOW_BUFFER_SIZE_EVENT) {
            COORD s = r[i].Event.WindowBufferSizeEvent.dwSize;
            printf("  window buffer size %d x %d\n", s.X, s.Y);
        } else if (r[i].EventType != FOCUS_EVENT && r[i].EventType != MENU_EVENT) {
            /* (Focus comes and goes with the real window; not counted.) */
            printf("  event type %d\n", r[i].EventType);
        }
    }
}

static void info(const char *label, HANDLE h)
{
    CONSOLE_SCREEN_BUFFER_INFO i;
    GetConsoleScreenBufferInfo(h, &i);
    printf("%s: size %d x %d, window %d,%d-%d,%d\n", label, i.dwSize.X, i.dwSize.Y, i.srWindow.Left, i.srWindow.Top, i.srWindow.Right, i.srWindow.Bottom);
}

/* Ctrl+C as typed: Ctrl down, C down and up, Ctrl up. */
static void type_ctrl_c(void)
{
    INPUT_RECORD r[4];
    key(&r[0], TRUE, VK_CONTROL, 0x1D, 0, LEFT_CTRL_PRESSED);
    key(&r[1], TRUE, 'C', 0x2E, 3, LEFT_CTRL_PRESSED);
    key(&r[2], FALSE, 'C', 0x2E, 3, LEFT_CTRL_PRESSED);
    key(&r[3], FALSE, VK_CONTROL, 0x1D, 0, 0);
    DWORD n = 0;
    log_len = 0;
    log_[0] = 0;
    ResetEvent(called);
    WriteConsoleInputW(in, r, 4, &n);
    DWORD w = WaitForSingleObject(called, 1000);
    Sleep(100);
    printf("typed ctrl+c: wrote %lu, %s, ran:%s\n", n, w == WAIT_OBJECT_0 ? "handled" : "no handler", log_);
}

int main(void)
{
    main_tid = GetCurrentThreadId();
    called = CreateEventW(NULL, TRUE, FALSE, NULL);
    FreeConsole();
    if (!AllocConsole())
        return 1;
    in = CreateFileW(L"CONIN$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    out = CreateFileW(L"CONOUT$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    setvbuf(stdout, NULL, _IONBF, 0);

    /* Whatever the parent left: Git Bash starts programs ignoring Ctrl+C. */
    SetConsoleCtrlHandler(NULL, FALSE);
    printf("--- handlers\n");
    printf("add first %d\n", SetConsoleCtrlHandler(first, TRUE));
    printf("add second %d\n", SetConsoleCtrlHandler(second, TRUE));
    raise_("ctrl+c", CTRL_C_EVENT);
    raise_("ctrl+break", CTRL_BREAK_EVENT);
    second_says = FALSE;
    raise_("second passes it on", CTRL_C_EVENT);
    second_says = TRUE;
    printf("remove first %d\n", SetConsoleCtrlHandler(first, FALSE));
    /* (MSVC evaluates arguments right to left: the error is read after.) */
    SetLastError(0);
    BOOL again = SetConsoleCtrlHandler(first, FALSE);
    DWORD err = GetLastError();
    printf("remove first again %d (error %lu)\n", again, err);
    raise_("second alone", CTRL_C_EVENT);
    SetLastError(0);
    BOOL bad = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0x7FFFFFF0);
    err = GetLastError();
    printf("generate to a bad group %d (error %lu)\n", bad, err);
    SetLastError(0);
    bad = GenerateConsoleCtrlEvent(7, 0);
    err = GetLastError();
    printf("generate a bad event %d (error %lu)\n", bad, err);

    printf("--- ignoring ctrl+c\n");
    printf("ignore %d\n", SetConsoleCtrlHandler(NULL, TRUE));
    raise_("ctrl+c ignored", CTRL_C_EVENT);
    raise_("ctrl+break still", CTRL_BREAK_EVENT);
    printf("stop ignoring %d\n", SetConsoleCtrlHandler(NULL, FALSE));
    raise_("ctrl+c again", CTRL_C_EVENT);

    printf("--- typed\n");
    DWORD mode = 0;
    GetConsoleMode(in, &mode);
    printf("input mode %lx\n", mode);
    FlushConsoleInputBuffer(in);
    type_ctrl_c();
    show_input("processed on");
    SetConsoleMode(in, mode & ~ENABLE_PROCESSED_INPUT);
    type_ctrl_c();
    show_input("processed off");
    SetConsoleMode(in, mode);

    printf("--- window size events\n");
    HANDLE buf = CreateConsoleScreenBuffer(GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, CONSOLE_TEXTMODE_BUFFER, NULL);
    SetConsoleActiveScreenBuffer(buf);
    FlushConsoleInputBuffer(in);
    info("new buffer", buf);
    show_input("after making it active");
    SMALL_RECT tiny = {0, 0, 9, 4};
    printf("small window %d\n", SetConsoleWindowInfo(buf, TRUE, &tiny));
    info("then", buf);
    show_input("after");
    COORD a = {50, 20};
    printf("resize without window input %d\n", SetConsoleScreenBufferSize(buf, a));
    show_input("after");
    SetConsoleMode(in, mode | ENABLE_WINDOW_INPUT);
    COORD b = {60, 25};
    printf("resize with window input %d\n", SetConsoleScreenBufferSize(buf, b));
    printf("same size again %d\n", SetConsoleScreenBufferSize(buf, b));
    show_input("after");
    SetConsoleActiveScreenBuffer(out);
    SetConsoleMode(in, mode);

    printf("done\n");
    return 9;
}
