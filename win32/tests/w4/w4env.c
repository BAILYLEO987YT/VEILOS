// W4 test: command line, environment, modules, LoadLibrary, GetProcAddress.
// Run with arguments: RUN /TESTS/W4ENV.EXE alpha "two words"
#include "t.h"

typedef int (*add_fn)(int, int);
typedef int (*ord_fn)(void);

void start(void) {
    char buf[512];

    const char *cl = GetCommandLineA();
    out("command line: ");
    out(cl);
    out("\r\n");
    int has_args = 0;
    for (const char *p = cl; *p; p++) if (p[0] == 'a' && p[1] == 'l' && p[2] == 'p' && p[3] == 'h' && p[4] == 'a') has_args = 1;
    check("command line has the arguments", has_args);
    const WCHAR *wcl = GetCommandLineW();
    check("GetCommandLineW starts with the quoted program", wcl[0] == '"' && wcl[1] == 'C' && wcl[2] == ':');

    check("GetEnvironmentVariable OS", GetEnvironmentVariableA("OS", buf, sizeof buf) == 10 && seq(buf, "Windows_NT"));
    check("names ignore case", GetEnvironmentVariableA("os", buf, sizeof buf) == 10);
    check_n("missing variable: ERROR_ENVVAR_NOT_FOUND", GetEnvironmentVariableA("NO_SUCH_VAR", buf, sizeof buf) == 0 && GetLastError() == 203, GetLastError());
    check_n("too small a buffer returns the size needed", GetEnvironmentVariableA("OS", buf, 4) == 11, GetEnvironmentVariableA("OS", buf, 4));
    check("SetEnvironmentVariable", SetEnvironmentVariableA("VEIL_TEST", "hello"));
    check("... and read back", GetEnvironmentVariableA("VEIL_TEST", buf, sizeof buf) == 5 && seq(buf, "hello"));
    check("ExpandEnvironmentStrings", ExpandEnvironmentStringsA("%VEIL_TEST%-%OS%-%NOPE%", buf, sizeof buf) && seq(buf, "hello-Windows_NT-%NOPE%"));
    SetEnvironmentVariableA("VEIL_TEST", 0);
    check("variable removed", GetEnvironmentVariableA("VEIL_TEST", buf, sizeof buf) == 0);
    char *block = GetEnvironmentStringsA();
    int entries = 0;
    for (char *p = block; *p; p += slen(p) + 1) entries++;
    check_n("GetEnvironmentStrings", entries >= 10, entries);
    FreeEnvironmentStringsA(block);

    HMODULE self = GetModuleHandleA(0);
    check("GetModuleHandle(NULL) is this EXE", self && ((IMAGE_DOS_HEADER *)self)->e_magic == IMAGE_DOS_SIGNATURE);
    GetModuleFileNameA(0, buf, sizeof buf);
    out("module file name: ");
    out(buf);
    out("\r\n");
    check("GetModuleFileName ends in W4ENV.EXE", slen(buf) > 9 && (seq(buf + slen(buf) - 9, "w4env.exe") || seq(buf + slen(buf) - 9, "W4ENV.EXE")));
    HMODULE k32 = GetModuleHandleA("kernel32");
    check("GetModuleHandle(kernel32)", k32 != 0);
    check("GetProcAddress(kernel32, GetTickCount) is the import", (void *)GetProcAddress(k32, "GetTickCount") == (void *)GetTickCount);

    HMODULE lib = LoadLibraryA("w1lib.dll");
    check("LoadLibrary w1lib.dll (and its import w1dep.dll)", lib != 0);
    add_fn add = (add_fn)GetProcAddress(lib, "lib_add");
    check("GetProcAddress lib_add, called", add && add(2, 3) == 1005);
    int *counter = (int *)GetProcAddress(lib, "lib_counter");
    check("its DllMain ran (lib_counter == 42)", counter && *counter == 42);
    add_fn mul = (add_fn)GetProcAddress(lib, "lib_mul");
    check("forwarded export through GetProcAddress", mul && mul(6, 7) == 42);
    ord_fn ord = (ord_fn)GetProcAddress(lib, (LPCSTR)5);
    check("GetProcAddress by ordinal", ord && ord() == 55);
    check("loading it again returns the same module", LoadLibraryA("W1LIB.DLL") == lib);
    check_n("missing export: ERROR_PROC_NOT_FOUND", GetProcAddress(lib, "nope") == 0 && GetLastError() == 127, GetLastError());
    check_n("missing DLL: ERROR_MOD_NOT_FOUND", LoadLibraryA("nosuch.dll") == 0 && GetLastError() == 126, GetLastError());
    HMODULE u32 = LoadLibraryA("user32.dll");
    check("a DLL that only exists as kernel built-ins loads", u32 != 0);
    check("... and GetProcAddress finds its functions", GetProcAddress(u32, "DefWindowProcW") != 0);

    done("w4env");
}
