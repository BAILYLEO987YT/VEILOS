/* W21 (S5): the console as a grid. A screen buffer of a known size, written
 * with the Win32 calls and with escape sequences, read back cell by cell; and
 * input: key events in, read as lines, as characters, as escape sequences.
 * Runs in a console of its own (AllocConsole); prints to stdout, which the
 * caller redirects to a file. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static HANDLE out, in, buf;
static const int W = 40, H = 12;

/* Row `y` of the buffer as text, trailing spaces cut. */
static void row(int y)
{
    wchar_t line[128];
    DWORD n = 0;
    COORD at = {0, (SHORT)y};
    ReadConsoleOutputCharacterW(buf, line, W, at, &n);
    while (n > 0 && line[n - 1] == L' ')
        n--;
    /* Anything outside ASCII as its code, so the C locale doesn't matter. */
    printf("  %2d|", y);
    for (DWORD i = 0; i < n; i++)
        if (line[i] >= 0x20 && line[i] < 0x7F)
            putchar(line[i]);
        else
            printf("\\x%04x", line[i]);
    printf("|\n");
}

static void rows(int from, int to)
{
    for (int y = from; y <= to; y++)
        row(y);
}

/* Attributes of a few cells, in hex. */
static void attrs(int y, int x0, int count)
{
    WORD a[64];
    DWORD n = 0;
    COORD at = {(SHORT)x0, (SHORT)y};
    ReadConsoleOutputAttribute(buf, a, count, at, &n);
    printf("  attrs %d,%d:", y, x0);
    for (DWORD i = 0; i < n; i++)
        printf(" %02x", a[i]);
    printf("\n");
}

static void cursor(const char *label)
{
    CONSOLE_SCREEN_BUFFER_INFO info;
    GetConsoleScreenBufferInfo(buf, &info);
    printf("  cursor %s: %d,%d attr %02x\n", label, info.dwCursorPosition.X, info.dwCursorPosition.Y, info.wAttributes);
}

static void clear(void)
{
    DWORD n;
    COORD home = {0, 0};
    FillConsoleOutputCharacterW(buf, L' ', W * H, home, &n);
    FillConsoleOutputAttribute(buf, 0x07, W * H, home, &n);
    SetConsoleTextAttribute(buf, 0x07);
    SetConsoleCursorPosition(buf, home);
}

static void put(const wchar_t *text)
{
    DWORD n;
    WriteConsoleW(buf, text, (DWORD)wcslen(text), &n, NULL);
}

static void win32_calls(void)
{
    printf("--- the grid through Win32 calls\n");
    CONSOLE_SCREEN_BUFFER_INFO info;
    GetConsoleScreenBufferInfo(buf, &info);
    printf("size %dx%d window %d,%d-%d,%d attr %02x\n", info.dwSize.X, info.dwSize.Y, info.srWindow.Left, info.srWindow.Top,
           info.srWindow.Right, info.srWindow.Bottom, info.wAttributes);
    clear();
    put(L"hello\nsecond line\r\nthird");
    cursor("after text");
    put(L"\rTHI");
    rows(0, 3);
    /* Past the right edge: the line wraps. */
    clear();
    put(L"0123456789012345678901234567890123456789ABC");
    cursor("after wrapping");
    rows(0, 1);
    /* Exactly the width: the cursor waits at the edge, unwrapped. */
    clear();
    put(L"0123456789012345678901234567890123456789");
    cursor("at the edge");
    put(L"Z");
    cursor("after one more");
    rows(0, 1);
    clear();
    put(L"a\tb\tc");
    cursor("after tabs");
    put(L"\bX");
    rows(0, 0);
    clear();
    COORD at = {10, 2};
    SetConsoleCursorPosition(buf, at);
    SetConsoleTextAttribute(buf, 0x1E);
    put(L"colour");
    SetConsoleTextAttribute(buf, 0x07);
    cursor("after colour");
    rows(2, 2);
    attrs(2, 8, 10);
    DWORD n;
    COORD f = {3, 4};
    FillConsoleOutputCharacterW(buf, L'#', 5, f, &n);
    printf("filled %lu\n", n);
    FillConsoleOutputAttribute(buf, 0x4F, 3, f, &n);
    COORD w = {0, 5};
    WriteConsoleOutputCharacterW(buf, L"direct", 6, w, &n);
    printf("wrote %lu\n", n);
    rows(4, 5);
    attrs(4, 2, 7);
    cursor("unmoved by them");
    /* Scrolling a block. */
    clear();
    for (int y = 0; y < 6; y++) {
        wchar_t line[32];
        swprintf(line, 32, L"line %d\n", y);
        put(line);
    }
    SMALL_RECT r = {0, 1, 9, 4};
    COORD to = {2, 0};
    CHAR_INFO fill = {{L'.'}, 0x07};
    ScrollConsoleScreenBufferW(buf, &r, NULL, to, &fill);
    rows(0, 5);
    /* Writing past the last row scrolls the whole buffer up. */
    clear();
    for (int y = 0; y < H + 2; y++) {
        wchar_t line[32];
        swprintf(line, 32, L"row %d\n", y);
        put(line);
    }
    cursor("after scrolling");
    rows(0, 1);
    rows(H - 2, H - 1);
}

static void put_vt(const char *text)
{
    DWORD n;
    WriteConsoleA(buf, text, (DWORD)strlen(text), &n, NULL);
}

static void escapes(void)
{
    printf("--- escape sequences\n");
    DWORD mode = 0;
    GetConsoleMode(buf, &mode);
    printf("mode before: %lx\n", mode);
    printf("VT on: %d\n", SetConsoleMode(buf, ENABLE_PROCESSED_OUTPUT | ENABLE_WRAP_AT_EOL_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING));
    clear();
    put_vt("\x1b[2J\x1b[H");
    put_vt("top\x1b[3;5Hat 3,5\x1b[1;1Hx");
    cursor("after CUP");
    put_vt("\x1b[5;10H\x1b[2Aup\x1b[3Bdown\x1b[4Dleft\x1b[2Cright");
    rows(0, 7);
    clear();
    put_vt("\x1b[H0123456789\x1b[1;5H\x1b[K");
    put_vt("\x1b[2;1Habcdefghij\x1b[2;5H\x1b[1K");
    put_vt("\x1b[3;1Hklmnopqrst\x1b[3;3H\x1b[2K");
    put_vt("\x1b[4;1Huvwxyz\x1b[4;2H\x1b[2X");
    rows(0, 3);
    clear();
    put_vt("\x1b[Hred\x1b[31m RED \x1b[1;32mGREEN\x1b[0m plain \x1b[7mINV\x1b[27m\x1b[44mbg\x1b[49m.");
    rows(0, 0);
    attrs(0, 0, 25);
    put_vt("\x1b[2;1H\x1b[38;5;196mA\x1b[38;2;0;0;255mB\x1b[0m\x1b[93mC\x1b[0m");
    attrs(1, 0, 3);
    clear();
    put_vt("\x1b[H12345\x1b[1;2H\x1b[2@\x1b[1;9H\x1b[P");
    rows(0, 0);
    clear();
    for (int y = 0; y < 6; y++) {
        char line[32];
        sprintf(line, "\x1b[%d;1Hline %d", y + 1, y);
        put_vt(line);
    }
    put_vt("\x1b[3;1H\x1b[L\x1b[5;1H\x1b[M");
    rows(0, 5);
    clear();
    for (int y = 0; y < 6; y++) {
        char line[32];
        sprintf(line, "\x1b[%d;1Hreg %d", y + 1, y);
        put_vt(line);
    }
    put_vt("\x1b[2;4r\x1b[4;1H\n\nnew\x1b[r");
    rows(0, 5);
    clear();
    put_vt("\x1b[3;7Hsaved\x1b" "7\x1b[Hhome\x1b" "8back");
    cursor("after restore");
    put_vt("\x1b[?25l");
    CONSOLE_CURSOR_INFO ci;
    GetConsoleCursorInfo(buf, &ci);
    printf("cursor shown after ?25l: %d\n", ci.bVisible);
    put_vt("\x1b[?25h");
    GetConsoleCursorInfo(buf, &ci);
    printf("cursor shown after ?25h: %d\n", ci.bVisible);
    clear();
    put_vt("\x1b[H0123456789012345678901234567890123456789");
    cursor("VT at the edge");
    put_vt("\r\n");
    cursor("then CRLF");
    put_vt("\x1b]0;a title\x07" "after");
    wchar_t title[64] = L"";
    GetConsoleTitleW(title, 64);
    printf("title: %ls\n", title);
    rows(0, 1);
    SetConsoleMode(buf, ENABLE_PROCESSED_OUTPUT | ENABLE_WRAP_AT_EOL_OUTPUT);
    clear();
    put_vt("raw\x1b[31mesc");
    rows(0, 0);
}

static void key(WORD vk, wchar_t ch, DWORD state, INPUT_RECORD *r)
{
    memset(r, 0, sizeof *r);
    r->EventType = KEY_EVENT;
    r->Event.KeyEvent.bKeyDown = TRUE;
    r->Event.KeyEvent.wRepeatCount = 1;
    r->Event.KeyEvent.wVirtualKeyCode = vk;
    r->Event.KeyEvent.uChar.UnicodeChar = ch;
    r->Event.KeyEvent.dwControlKeyState = state;
}

static void send_keys(const wchar_t *text)
{
    INPUT_RECORD r[64];
    DWORD n = 0, k = 0;
    for (; text[k]; k++) {
        wchar_t c = text[k];
        WORD vk = c == L'\r' ? VK_RETURN : c == L'\b' ? VK_BACK : (WORD)towupper(c);
        key(vk, c, 0, &r[n]);
        n++;
    }
    DWORD written = 0;
    WriteConsoleInputW(in, r, n, &written);
}

static void input(void)
{
    printf("--- input\n");
    FlushConsoleInputBuffer(in);
    DWORD mode = 0;
    GetConsoleMode(in, &mode);
    printf("input mode: %lx\n", mode);
    /* Line mode: a line once Enter comes, with the editing done. */
    SetConsoleMode(in, ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
    send_keys(L"helo\bxo world\r");
    DWORD count = 0;
    GetNumberOfConsoleInputEvents(in, &count);
    printf("events waiting: %lu\n", count);
    wchar_t line[64];
    DWORD got = 0;
    ReadConsoleW(in, line, 64, &got, NULL);
    line[got] = 0;
    printf("line: %lu [", got);
    for (DWORD i = 0; i < got; i++)
        printf(line[i] < 32 ? "\\x%02x" : "%lc", line[i]);
    printf("]\n");
    /* Raw: characters as they come. */
    SetConsoleMode(in, 0);
    send_keys(L"ab");
    got = 0;
    ReadConsoleW(in, line, 64, &got, NULL);
    printf("raw read: %lu\n", got);
    FlushConsoleInputBuffer(in);
    /* Key events themselves. */
    INPUT_RECORD r[4];
    key(VK_UP, 0, 0, &r[0]);
    key('Q', L'q', LEFT_CTRL_PRESSED, &r[1]);
    DWORD written;
    WriteConsoleInputW(in, r, 2, &written);
    INPUT_RECORD seen[4];
    DWORD peeked = 0;
    PeekConsoleInputW(in, seen, 4, &peeked);
    printf("peeked %lu, still %lu\n", peeked, (GetNumberOfConsoleInputEvents(in, &count), count));
    DWORD read = 0;
    ReadConsoleInputW(in, seen, 4, &read);
    for (DWORD i = 0; i < read; i++)
        printf("event %lu: type %d down %d vk %02x char %04x state %lx\n", i, seen[i].EventType, seen[i].Event.KeyEvent.bKeyDown,
               seen[i].Event.KeyEvent.wVirtualKeyCode, seen[i].Event.KeyEvent.uChar.UnicodeChar, seen[i].Event.KeyEvent.dwControlKeyState);
    /* VT input: keys come out as the sequences a terminal sends. */
    SetConsoleMode(in, ENABLE_VIRTUAL_TERMINAL_INPUT);
    key(VK_UP, 0, 0, &r[0]);
    key(VK_RIGHT, 0, 0, &r[1]);
    key('Z', L'z', 0, &r[2]);
    WriteConsoleInputW(in, r, 3, &written);
    got = 0;
    ReadConsoleW(in, line, 64, &got, NULL);
    printf("as VT: %lu [", got);
    for (DWORD i = 0; i < got; i++)
        printf(line[i] < 32 ? "\\x%02x" : "%lc", line[i]);
    printf("]\n");
}

int main(void)
{
    FreeConsole();
    if (!AllocConsole())
        return 1;
    out = CreateFileW(L"CONOUT$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    in = CreateFileW(L"CONIN$", GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    /* A buffer of its own, of a known size, whatever the console window is. */
    buf = CreateConsoleScreenBuffer(GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, CONSOLE_TEXTMODE_BUFFER, NULL);
    SetConsoleActiveScreenBuffer(buf);
    SMALL_RECT tiny = {0, 0, 9, 4};
    SetConsoleWindowInfo(buf, TRUE, &tiny);
    COORD size = {(SHORT)W, (SHORT)H};
    BOOL sized = SetConsoleScreenBufferSize(buf, size);
    SMALL_RECT window = {0, 0, (SHORT)(W - 1), (SHORT)(H - 1)};
    BOOL windowed = SetConsoleWindowInfo(buf, TRUE, &window);
    printf("buffer: %d sized %d windowed %d\n", buf != INVALID_HANDLE_VALUE, sized, windowed);
    win32_calls();
    escapes();
    input();
    printf("done\n");
    return 8;
}
