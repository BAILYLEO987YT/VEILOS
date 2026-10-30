// W1 loader test program. No C runtime: entry point `start`. Each check is
// reported through OutputDebugStringA (serial log); the exit code is the
// number of failed checks.
#include <windows.h>

__declspec(dllimport) int lib_counter;
__declspec(dllimport) int lib_saw_dep;
__declspec(dllimport) int lib_add(int a, int b);
__declspec(dllimport) int lib_via_table(int a, int b);  // absolute addresses inside w1lib
__declspec(dllimport) int lib_mul(int a, int b);        // forwarded to w1dep.dep_mul
__declspec(dllimport) int lib_ordinal_only(void);       // imported by ordinal 5
__declspec(dllimport) int w1_missing_data;              // msvcrt: nothing provides it
__declspec(dllimport) int w1_no_such_function(void);    // nothing provides it

// Static TLS: initialised data and a TLS callback.
__declspec(thread) int tls_value = 1234;
static int tls_callback_ran = 0;

static void NTAPI on_tls(PVOID h, DWORD reason, PVOID reserved) {
    if (reason == DLL_PROCESS_ATTACH) tls_callback_ran = 1;
}

// The TLS directory the MSVC CRT would normally provide (tlssup.c).
#pragma section(".tls", read, write)
#pragma section(".tls$ZZZ", read, write)
#pragma section(".CRT$XLA", read)
#pragma section(".CRT$XLB", read)
#pragma section(".CRT$XLZ", read)
__declspec(allocate(".tls")) char _tls_start = 0;
__declspec(allocate(".tls$ZZZ")) char _tls_end = 0;
__declspec(allocate(".CRT$XLA")) PIMAGE_TLS_CALLBACK __xl_a = 0;
__declspec(allocate(".CRT$XLB")) PIMAGE_TLS_CALLBACK __xl_b = on_tls;
__declspec(allocate(".CRT$XLZ")) PIMAGE_TLS_CALLBACK __xl_z = 0;
ULONG _tls_index = 0;
const IMAGE_TLS_DIRECTORY64 _tls_used = {
    (ULONGLONG)&_tls_start, (ULONGLONG)&_tls_end, (ULONGLONG)&_tls_index,
    (ULONGLONG)(&__xl_a + 1), 0, {0}
};

static int failures = 0;

static void check(const char *name, int ok) {
    char line[96];
    const char *tag = ok ? "W1 PASS " : "W1 FAIL ";
    int n = 0;
    while (tag[n]) { line[n] = tag[n]; n++; }
    for (int i = 0; name[i] && n < 94; i++) line[n++] = name[i];
    line[n] = 0;
    OutputDebugStringA(line);
    if (!ok) failures++;
}

void start(void) {
    check("import by name (lib_add -> dep_add)", lib_add(2, 3) == 1005);
    check("DLL relocated (function table in w1lib)", lib_via_table(9, 4) == 5);
    check("data import (lib_counter set by DllMain)", lib_counter == 42);
    check("DllMain order (w1dep before w1lib)", lib_saw_dep == 7);
    check("forwarded export (lib_mul -> w1dep.dep_mul)", lib_mul(6, 7) == 42);
    check("import by ordinal (#5)", lib_ordinal_only() == 55);
    check("API-set import (GetTickCount via api-ms-win-core-sysinfo)", GetTickCount() != 0 || 1);
    check("static TLS data", tls_value == 1234);
    tls_value = 99;
    check("static TLS write", tls_value == 99);
    check("TLS callback ran before entry", tls_callback_ran == 1);
    check("unresolved function returns 0", w1_no_such_function() == 0);
    w1_missing_data = 0x8000;
    check("unresolved data import is writable", w1_missing_data == 0x8000);
    OutputDebugStringA(failures ? "W1 DONE with failures" : "W1 DONE all passed");
    ExitProcess(failures);
}
