/* W32 heap layout: where a process's heap, malloc and VirtualAlloc land,
 * and the same for a child it starts. Run it twice: in Security Mode (as on
 * Windows) the addresses differ from run to run; a child's start where its
 * parent's did. Prints addresses, so there is no expected output: compare
 * runs. */
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void show(const char *who)
{
    void *m = malloc(100);
    void *v = VirtualAlloc(NULL, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    HANDLE h = HeapCreate(0, 0, 0);
    printf("%s: process heap %p malloc %p VirtualAlloc %p HeapCreate %p\n", who, (void *)GetProcessHeap(), m, v, (void *)h);
    fflush(stdout);
}

int main(int argc, char **argv)
{
    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    char line[MAX_PATH + 16];
    if (argc > 1 && !strcmp(argv[1], "child")) {
        show("child ");
        return 0;
    }
    show("parent");
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    snprintf(line, sizeof line, "\"%s\" child", argv[0]);
    if (CreateProcessA(NULL, line, NULL, NULL, TRUE, 0, NULL, NULL, &si, &pi)) {
        WaitForSingleObject(pi.hProcess, 30000);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    } else {
        printf("CreateProcess: error %lu\n", GetLastError());
    }
    return 0;
}
