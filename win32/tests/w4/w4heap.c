// W4 test: heaps and virtual memory.
#include "t.h"

void start(void) {
    HANDLE heap = GetProcessHeap();
    check("process heap exists", heap != 0);

    // Many small blocks of varied sizes, written and checked, then freed.
    static unsigned char *blocks[2000];
    int ok = 1;
    for (int i = 0; i < 2000; i++) {
        SIZE_T n = 1 + (i * 37) % 700;
        blocks[i] = HeapAlloc(heap, 0, n);
        if (!blocks[i] || ((ULONG_PTR)blocks[i] & 15)) { ok = 0; break; }
        memset(blocks[i], i & 0xFF, n);
    }
    for (int i = 0; ok && i < 2000; i++) {
        SIZE_T n = 1 + (i * 37) % 700;
        if (HeapSize(heap, 0, blocks[i]) != n || blocks[i][n - 1] != (unsigned char)(i & 0xFF)) ok = 0;
    }
    check("2000 blocks: 16-aligned, sizes and contents kept", ok);
    for (int i = 0; i < 2000; i += 2) HeapFree(heap, 0, blocks[i]);
    void *reuse = HeapAlloc(heap, 0, 38);
    check("freed blocks are reused", reuse != 0);
    for (int i = 1; i < 2000; i += 2) HeapFree(heap, 0, blocks[i]);

    unsigned char *z = HeapAlloc(heap, HEAP_ZERO_MEMORY, 4000);
    int zero = 1;
    for (int i = 0; i < 4000; i++) if (z[i]) zero = 0;
    check("HEAP_ZERO_MEMORY", zero);

    unsigned char *r = HeapAlloc(heap, 0, 16);
    for (int i = 0; i < 16; i++) r[i] = (unsigned char)i;
    r = HeapReAlloc(heap, 0, r, 5000);
    int kept = r != 0;
    for (int i = 0; kept && i < 16; i++) if (r[i] != i) kept = 0;
    check("HeapReAlloc grows and keeps contents", kept && HeapSize(heap, 0, r) == 5000);

    unsigned char *big = HeapAlloc(heap, 0, 3 * 1024 * 1024);
    if (big) { big[0] = 1; big[3 * 1024 * 1024 - 1] = 2; }
    check("3 MiB block", big && big[3 * 1024 * 1024 - 1] == 2);
    check("HeapFree of the 3 MiB block", HeapFree(heap, 0, big));

    HANDLE own = HeapCreate(0, 0, 0);
    void *p = HeapAlloc(own, 0, 100);
    check("HeapCreate + alloc", own && p);
    check("HeapDestroy", HeapDestroy(own));

    char *v = VirtualAlloc(0, 3 * 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    int vz = v != 0;
    for (int i = 0; vz && i < 3 * 4096; i++) if (v[i]) vz = 0;
    check("VirtualAlloc: zeroed pages", vz);
    v[0] = 'x';
    MEMORY_BASIC_INFORMATION mbi;
    SIZE_T q = VirtualQuery(v, &mbi, sizeof mbi);
    check("VirtualQuery: committed region", q == sizeof mbi && mbi.State == MEM_COMMIT && mbi.BaseAddress == v);
    DWORD old = 0;
    check("VirtualProtect", VirtualProtect(v, 4096, PAGE_READONLY, &old) && old == PAGE_READWRITE);
    check("VirtualFree", VirtualFree(v, 0, MEM_RELEASE));

    HLOCAL l = LocalAlloc(LMEM_ZEROINIT, 64);
    check("LocalAlloc / LocalFree", l && ((char *)l)[63] == 0 && LocalFree(l) == 0);

    SYSTEM_INFO si;
    GetSystemInfo(&si);
    check_n("GetSystemInfo: page size and CPUs", si.dwPageSize == 4096 && si.dwNumberOfProcessors >= 1, si.dwNumberOfProcessors);

    done("w4heap");
}
