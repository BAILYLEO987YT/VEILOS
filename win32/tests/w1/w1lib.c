// W1 loader test: a DLL with its own import (w1dep), a data export, a forwarder
// (lib_mul -> w1dep.dep_mul, see w1lib.def) and an ordinal-only export.
#include <windows.h>

__declspec(dllimport) int dep_value;
__declspec(dllimport) int dep_add(int a, int b);

int lib_counter = 0;     // DllMain sets it to 42
int lib_saw_dep = 0;     // dep_value as seen from DllMain: 7 if w1dep was initialised first

int lib_add(int a, int b) { return dep_add(a, b) + 1000; }

// A table of absolute addresses: only right if the loader relocated the DLL.
static int local_sub(int a, int b) { return a - b; }
static int (*const ops[])(int, int) = { local_sub };
int lib_via_table(int a, int b) { return ops[0](a, b); }
int lib_ordinal_only(void) { return 55; }

BOOL WINAPI DllMain(HINSTANCE h, DWORD reason, LPVOID reserved) {
    if (reason == DLL_PROCESS_ATTACH) {
        lib_counter = 42;
        lib_saw_dep = dep_value;
    }
    return TRUE;
}
