// W1 loader test: the dependency at the bottom of the chain (w1test.exe -> w1lib.dll -> w1dep.dll).
#include <windows.h>

__declspec(dllexport) int dep_value = 1;    // DllMain sets it to 7

__declspec(dllexport) int dep_add(int a, int b) { return a + b; }
__declspec(dllexport) int dep_mul(int a, int b) { return a * b; }   // reached through a forwarder in w1lib

BOOL WINAPI DllMain(HINSTANCE h, DWORD reason, LPVOID reserved) {
    if (reason == DLL_PROCESS_ATTACH) dep_value = 7;
    return TRUE;
}
