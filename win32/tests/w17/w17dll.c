/* Loaded by w17io through its full path. It has thread-local data and a TLS
   callback, as a Rust proc macro DLL has. */
#include <windows.h>

__declspec(dllexport) int w17_data = 7;
__declspec(dllexport) int w17_answer(void)
{
    return 42;
}

__declspec(thread) int w17_tls = 5;
__declspec(dllexport) int w17_tls_get(void)
{
    return w17_tls;
}
__declspec(dllexport) void w17_tls_set(int v)
{
    w17_tls = v;
}

__declspec(dllexport) int w17_callback_attaches = 0;
static void NTAPI callback(PVOID module, DWORD reason, PVOID reserved)
{
    (void)module;
    (void)reserved;
    if (reason == DLL_PROCESS_ATTACH)
        w17_callback_attaches++;
}
#pragma comment(linker, "/INCLUDE:_tls_used")
#pragma comment(linker, "/INCLUDE:w17_callback")
#pragma section(".CRT$XLB", read)
__declspec(allocate(".CRT$XLB")) const PIMAGE_TLS_CALLBACK w17_callback = callback;
