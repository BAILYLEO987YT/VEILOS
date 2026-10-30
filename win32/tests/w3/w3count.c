// W3 test: a console program that runs for a few seconds, printing a tick
// every second, so several copies can be seen running at once. Exits
// with 40 + its pid. No C runtime: entry point `start`.
#include <windows.h>

static void out(const char *s) {
    DWORD n = 0, len = 0;
    while (s[len]) len++;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, len, &n, 0);
}

static void out_num(DWORD v) {
    char buf[12];
    int i = 11;
    buf[i] = 0;
    do { buf[--i] = (char)('0' + v % 10); v /= 10; } while (v);
    out(buf + i);
}

void start(void) {
    DWORD pid = GetCurrentProcessId();
    for (int tick = 1; tick <= 10; tick++) {
        DWORD until = GetTickCount() + 1000;
        while (GetTickCount() < until) { }
        out("pid ");
        out_num(pid);
        out(" tick ");
        out_num(tick);
        out("\r\n");
    }
    ExitProcess(40 + pid);
}
