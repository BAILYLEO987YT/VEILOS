// W3 test: console input. Reads typed lines with ReadFile and echoes them
// until "quit" (or end of input); exits with the number of lines it got
// before "quit". No C runtime: entry point `start`.
#include <windows.h>

static void out(const char *s, DWORD len) {
    DWORD n = 0;
    if (len == 0) while (s[len]) len++;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, len, &n, 0);
}

void start(void) {
    HANDLE in = GetStdHandle(STD_INPUT_HANDLE);
    char line[256];
    DWORD lines = 0;
    out("type lines; quit ends\r\n", 0);
    for (;;) {
        out("echo> ", 0);
        DWORD got = 0;
        if (!ReadFile(in, line, sizeof line, &got, 0) || got == 0) break;
        while (got && (line[got - 1] == '\n' || line[got - 1] == '\r')) got--;
        if (got == 4 && line[0] == 'q' && line[1] == 'u' && line[2] == 'i' && line[3] == 't') break;
        lines++;
        out("you typed: ", 0);
        out(line, got);
        out("\r\n", 0);
    }
    ExitProcess(lines);
}
