// W2 test: console output through VEILOS's own kernel32.dll and ntdll.dll.
// No C runtime: entry point `start`. Expected window contents:
//
//   Hello from a Windows program on VEILOS
//   one line, written in two pieces
//
//   WriteConsoleW works
//   this line went to stderr
//   bad handle rejected: error 6
//
//   [exited with code 3]
#include <windows.h>

static DWORD len(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    return n;
}

static void out(HANDLE h, const char *s) {
    DWORD written = 0;
    WriteFile(h, s, len(s), &written, 0);
}

void start(void) {
    HANDLE o = GetStdHandle(STD_OUTPUT_HANDLE);
    HANDLE e = GetStdHandle(STD_ERROR_HANDLE);
    DWORD n = 0;

    out(o, "Hello from a Windows program on VEILOS\r\n");
    out(o, "one line, ");
    out(o, "written in two pieces\r\n");
    out(o, "\r\n");
    WriteConsoleW(o, L"WriteConsoleW works\r\n", 21, &n, 0);
    out(e, "this line went to stderr\r\n");

    BOOL ok = WriteFile((HANDLE)0x1234, "x", 1, &n, 0);
    DWORD err = GetLastError();
    char msg[] = "bad handle rejected: error ?\r\n";
    msg[len(msg) - 3] = (char)('0' + err % 10);
    out(o, ok ? "bad handle NOT rejected\r\n" : msg);

    ExitProcess(3);
}
