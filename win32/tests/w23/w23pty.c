/* W23 (S5c): a pseudo-console. The host (no arguments) makes one of 40x10 on
 * two pipes and starts itself in it as the child ("child"); it types into the
 * input pipe as a terminal would (text, Enter, an arrow key), resizes it to
 * 50x12, and reads everything the pseudo-console sends. Its exact bytes are
 * the console host's business (when it repaints, how it moves the cursor), so
 * the host runs them through a small terminal of its own and prints the screen
 * they leave: that, with the child's exit code, is what must match. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

/* -----| the child |----- */

static HANDLE cin_, cout_;

static void say(const char *s)
{
    DWORD n;
    WriteConsoleA(cout_, s, (DWORD)strlen(s), &n, NULL);
}

static int child(void)
{
    cin_ = GetStdHandle(STD_INPUT_HANDLE);
    cout_ = GetStdHandle(STD_OUTPUT_HANDLE);
    CONSOLE_SCREEN_BUFFER_INFO i;
    GetConsoleScreenBufferInfo(cout_, &i);
    char line[256];
    int before_w = i.dwSize.X, before_h = i.srWindow.Bottom - i.srWindow.Top + 1;
    say("child up\r\n");
    say("\x1b[31m" "red" "\x1b[m" " plain " "\x1b[1;32m" "green" "\x1b[m\r\n");
    /* The Win32 calls too: text put at a place, then the cursor back. */
    COORD at = {30, 3};
    GetConsoleScreenBufferInfo(cout_, &i);
    COORD back = i.dwCursorPosition;
    SetConsoleCursorPosition(cout_, at);
    say("placed");
    SetConsoleCursorPosition(cout_, back);
    /* A line typed at the terminal, edited and echoed by the console. */
    say("name? ");
    wchar_t typed[64];
    DWORD got = 0;
    ReadConsoleW(cin_, typed, 63, &got, NULL);
    while (got && (typed[got - 1] == L'\r' || typed[got - 1] == L'\n'))
        got--;
    typed[got] = 0;
    /* Keys: the terminal's escape sequences come as key events. */
    DWORD mode;
    GetConsoleMode(cin_, &mode);
    SetConsoleMode(cin_, ENABLE_WINDOW_INPUT);
    say("keys?\r\n");
    int vk_seen = 0, resized_w = 0, resized_h = 0;
    while (!resized_w) {
        INPUT_RECORD r;
        DWORD n;
        if (!ReadConsoleInputW(cin_, &r, 1, &n))
            break;
        if (r.EventType == KEY_EVENT && r.Event.KeyEvent.bKeyDown && !vk_seen) {
            vk_seen = r.Event.KeyEvent.wVirtualKeyCode;
            say("resize?\r\n");
        } else if (r.EventType == WINDOW_BUFFER_SIZE_EVENT && vk_seen) {
            GetConsoleScreenBufferInfo(cout_, &i);
            resized_w = i.srWindow.Right - i.srWindow.Left + 1;
            resized_h = i.srWindow.Bottom - i.srWindow.Top + 1;
        }
    }
    SetConsoleMode(cin_, mode);
    sprintf(line, "typed [%ls]\r\nkey vk %02x\r\nsize %dx%d then %dx%d\r\n", typed, vk_seen, before_w, before_h, resized_w, resized_h);
    say(line);
    for (int k = 1; k <= 4; k++) {
        sprintf(line, "line %d\r\n", k);
        say(line);
    }
    say("end");
    return 23;
}

/* -----| the host's terminal: enough of one to replay what comes out |----- */

static int W = 40, H = 10;
static char scr[64][128];
static char fg[64][128];
static int cx, cy, wrap, colour = '.';

static void clear_rows(int from, int to)
{
    for (int y = from; y <= to && y < H; y++) {
        memset(scr[y], ' ', sizeof scr[y]);
        memset(fg[y], '.', sizeof fg[y]);
    }
}

static void scroll_up(void)
{
    memmove(scr[0], scr[1], sizeof scr[0] * (H - 1));
    memmove(fg[0], fg[1], sizeof fg[0] * (H - 1));
    clear_rows(H - 1, H - 1);
}

static void put(char c)
{
    if (wrap) {
        cx = 0;
        wrap = 0;
        if (++cy == H) {
            scroll_up();
            cy = H - 1;
        }
    }
    scr[cy][cx] = c;
    fg[cy][cx] = (char)colour;
    if (cx == W - 1)
        wrap = 1;
    else
        cx++;
}

static void sgr(int *p, int n)
{
    if (n == 0)
        colour = '.';
    for (int i = 0; i < n; i++) {
        int v = p[i];
        if (v == 0 || v == 39)
            colour = '.';
        else if (v >= 30 && v <= 37)
            colour = '0' + (v - 30);
        else if (v >= 90 && v <= 97)
            colour = 'a' + (v - 90);
        else if (v == 38 && i + 2 < n && p[i + 1] == 5) {
            colour = p[i + 2] < 8 ? '0' + p[i + 2] : p[i + 2] < 16 ? 'a' + (p[i + 2] - 8) : '?';
            i += 2;
        } else if (v == 38 && i + 4 < n && p[i + 1] == 2) {
            colour = '?';
            i += 4;
        } else if (v == 48 && i + 1 < n) {
            i += p[i + 1] == 5 ? 2 : 4;
        }
    }
}

static int unknown;

static void replay(const unsigned char *b, size_t len)
{
    size_t i = 0;
    while (i < len) {
        unsigned char c = b[i++];
        if (c == 0x1b && i < len) {
            unsigned char k = b[i++];
            if (k == '[') {
                int p[16] = {0}, n = 0, priv = 0, have = 0;
                while (i < len && (b[i] == '?' || b[i] == '>' || b[i] == '=')) {
                    priv = 1;
                    i++;
                }
                while (i < len) {
                    if (b[i] >= '0' && b[i] <= '9') {
                        p[n] = p[n] * 10 + (b[i] - '0');
                        have = 1;
                        i++;
                    } else if (b[i] == ';' || b[i] == ':') {
                        if (n < 15)
                            n++;
                        i++;
                    } else
                        break;
                }
                if (have || n)
                    n++;
                if (i >= len)
                    break;
                while (i < len && b[i] >= 0x20 && b[i] <= 0x2F)
                    i++; /* intermediates */
                char f = (char)b[i++];
                int a = p[0] ? p[0] : 1;
                if (priv)
                    continue; /* modes: the cursor's visibility and the like */
                switch (f) {
                case 'H': case 'f':
                    cy = (p[0] ? p[0] : 1) - 1;
                    cx = (n > 1 && p[1] ? p[1] : 1) - 1;
                    wrap = 0;
                    break;
                case 'A': cy -= a; wrap = 0; break;
                case 'B': cy += a; wrap = 0; break;
                case 'C': cx += a; wrap = 0; break;
                case 'D': cx -= a; wrap = 0; break;
                case 'G': cx = a - 1; wrap = 0; break;
                case 'd': cy = a - 1; wrap = 0; break;
                case 'E': cy += a; cx = 0; wrap = 0; break;
                case 'F': cy -= a; cx = 0; wrap = 0; break;
                case 'J':
                    if (p[0] == 0) {
                        for (int x = cx; x < W; x++) { scr[cy][x] = ' '; fg[cy][x] = '.'; }
                        clear_rows(cy + 1, H - 1);
                    } else if (p[0] == 1) {
                        clear_rows(0, cy - 1);
                        for (int x = 0; x <= cx; x++) { scr[cy][x] = ' '; fg[cy][x] = '.'; }
                    } else
                        clear_rows(0, H - 1);
                    break;
                case 'K': {
                    int from = p[0] == 0 ? cx : 0, to = p[0] == 1 ? cx : W - 1;
                    for (int x = from; x <= to; x++) { scr[cy][x] = ' '; fg[cy][x] = '.'; }
                    break;
                }
                case 'X':
                    for (int x = cx; x < cx + a && x < W; x++) { scr[cy][x] = ' '; fg[cy][x] = '.'; }
                    break;
                case 'm': sgr(p, n); break;
                case 'S': for (int k2 = 0; k2 < a; k2++) scroll_up(); break;
                case 'r': case 't': case 'n': case 'c': case 'h': case 'l': case 'q': break;
                default: unknown++; break;
                }
                if (cx < 0) cx = 0;
                if (cy < 0) cy = 0;
                if (cx >= W) cx = W - 1;
                if (cy >= H) cy = H - 1;
            } else if (k == ']') {
                /* OSC (the title): to BEL or ST. */
                while (i < len && b[i] != 0x07 && !(b[i] == 0x1b && i + 1 < len && b[i + 1] == '\\'))
                    i++;
                i += (i < len && b[i] == 0x07) ? 1 : 2;
            } else if (k == '7' || k == '8' || k == '=' || k == '>') {
                /* save/restore cursor, keypad modes: unused by the console host here */
            } else if (k == '(' || k == ')') {
                i++;
            } else
                unknown++;
        } else if (c == '\r') {
            cx = 0;
            wrap = 0;
        } else if (c == '\n') {
            wrap = 0;
            if (++cy == H) {
                scroll_up();
                cy = H - 1;
            }
        } else if (c == '\b') {
            if (cx > 0) cx--;
            wrap = 0;
        } else if (c == 0x07) {
        } else if (c >= 0x20 && c < 0x7f) {
            put((char)c);
        } else if (c >= 0x80) {
            if ((c & 0xC0) != 0x80)
                put('?'); /* one cell for each character outside ASCII */
        }
    }
}

/* -----| the host |----- */

static unsigned char out_bytes[1 << 20];
static volatile LONG out_len;
static HANDLE out_read;

static DWORD WINAPI reader(void *unused)
{
    (void)unused;
    for (;;) {
        DWORD n = 0;
        if (out_len >= (LONG)sizeof out_bytes - 4096)
            break;
        if (!ReadFile(out_read, out_bytes + out_len, 4096, &n, NULL) || n == 0)
            break;
        InterlockedExchangeAdd(&out_len, (LONG)n);
    }
    return 0;
}

/* Wait until the output holds `marker` (from byte `from` on). */
static int wait_for(const char *marker, LONG from)
{
    size_t m = strlen(marker);
    for (int t = 0; t < 1000; t++) {
        LONG len = out_len;
        for (LONG i = from; i + (LONG)m <= len; i++)
            if (memcmp(out_bytes + i, marker, m) == 0)
                return 1;
        Sleep(10);
    }
    printf("never saw \"%s\"\n", marker);
    return 0;
}

static void type(HANDLE w, const char *s)
{
    DWORD n;
    WriteFile(w, s, (DWORD)strlen(s), &n, NULL);
}

int main(int argc, char **argv)
{
    if (argc > 1 && strcmp(argv[1], "child") == 0)
        return child();
    setvbuf(stdout, NULL, _IONBF, 0);

    HANDLE in_read, in_write, out_write;
    CreatePipe(&in_read, &in_write, NULL, 0);
    CreatePipe(&out_read, &out_write, NULL, 0);
    HPCON pc;
    COORD size = {40, 10};
    HRESULT hr = CreatePseudoConsole(size, in_read, out_write, 0, &pc);
    printf("create: %08lx\n", (unsigned long)hr);
    /* The pseudo-console has its own copies. */
    CloseHandle(in_read);
    CloseHandle(out_write);
    HANDLE t = CreateThread(NULL, 0, reader, NULL, 0, NULL);

    SIZE_T need = 0;
    InitializeProcThreadAttributeList(NULL, 1, 0, &need);
    LPPROC_THREAD_ATTRIBUTE_LIST list = (LPPROC_THREAD_ATTRIBUTE_LIST)HeapAlloc(GetProcessHeap(), 0, need);
    printf("attribute list: %d\n", InitializeProcThreadAttributeList(list, 1, 0, &need));
    printf("update: %d\n", UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, pc, sizeof pc, NULL, NULL));
    STARTUPINFOEXW si;
    memset(&si, 0, sizeof si);
    si.StartupInfo.cb = sizeof si;
    si.lpAttributeList = list;
    /* This program's own standard handles are a file: say so, or the child
     * would get them rather than the pseudo-console's. */
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    wchar_t self[MAX_PATH], cmd[MAX_PATH + 16];
    GetModuleFileNameW(NULL, self, MAX_PATH);
    swprintf(cmd, MAX_PATH + 16, L"\"%ls\" child", self);
    PROCESS_INFORMATION pi;
    BOOL ok = CreateProcessW(self, cmd, NULL, NULL, FALSE, EXTENDED_STARTUPINFO_PRESENT, NULL, NULL, &si.StartupInfo, &pi);
    printf("child started: %d\n", ok);
    if (!ok)
        return 1;

    int seen = wait_for("name? ", 0);
    type(in_write, "tyx\x7f\x7f" "yped\r");
    seen &= wait_for("keys?", 0);
    type(in_write, "\x1b[A");
    seen &= wait_for("resize?", 0);
    LONG resized_at = out_len;
    COORD bigger = {50, 12};
    hr = ResizePseudoConsole(pc, bigger);
    printf("resize: %08lx\n", (unsigned long)hr);
    WaitForSingleObject(pi.hProcess, 10000);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    printf("child exit: %lu\n", code);
    Sleep(300);
    ClosePseudoConsole(pc);
    WaitForSingleObject(t, 5000);
    DeleteProcThreadAttributeList(list);

    /* Replay what came out: at 40x10 until the resize, at 50x12 after. */
    for (int y = 0; y < 64; y++) {
        memset(scr[y], ' ', sizeof scr[y]);
        memset(fg[y], '.', sizeof fg[y]);
    }
    replay(out_bytes, (size_t)resized_at);
    W = 50;
    H = 12;
    for (int y = 0; y < H; y++)
        for (int x = 40; x < W; x++) { scr[y][x] = ' '; fg[y][x] = '.'; }
    replay(out_bytes + resized_at, (size_t)(out_len - resized_at));
    printf("markers seen: %d\n", seen);
    printf("screen (cursor %d,%d):\n", cx, cy);
    for (int y = 0; y < H; y++) {
        int n = W;
        while (n > 0 && scr[y][n - 1] == ' ')
            n--;
        printf("  %2d|%.*s|\n", y, n, scr[y]);
    }
    printf("colours:\n");
    for (int y = 0; y < H; y++) {
        int n = W;
        while (n > 0 && fg[y][n - 1] == '.')
            n--;
        if (n)
            printf("  %2d|%.*s|\n", y, n, fg[y]);
    }
    fprintf(stderr, "(%ld bytes, %d sequences not understood)\n", out_len, unknown);
    return 11;
}
