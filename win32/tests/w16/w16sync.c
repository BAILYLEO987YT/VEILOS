/* W16 (S1): what the toolchain synchronises with: named events, semaphores and
 * mutexes, WaitOnAddress, fibers (also with an exception and deep calls inside
 * one), named pipes, and nameless pipes made the way Rust's standard library
 * makes them. */
#include <windows.h>
#include <winternl.h>
#include <stdio.h>
#include <string.h>

/* -----| named objects |----- */

static void named(void)
{
    printf("--- named objects\n");
    HANDLE e = CreateEventA(NULL, TRUE, FALSE, "W16.Event");
    printf("event: %d err %lu\n", e != NULL, GetLastError());
    HANDLE e2 = CreateEventA(NULL, TRUE, FALSE, "W16.Event");
    printf("again: %d err %lu\n", e2 != NULL, GetLastError());
    HANDLE e3 = OpenEventA(EVENT_ALL_ACCESS, FALSE, "W16.Event");
    printf("open: %d\n", e3 != NULL);
    SetEvent(e);
    printf("set through one, wait on others: %lu %lu\n", WaitForSingleObject(e2, 0), WaitForSingleObject(e3, 0));
    ResetEvent(e3);
    printf("reset through another: %lu\n", WaitForSingleObject(e, 0));
    CloseHandle(e);
    CloseHandle(e2);
    CloseHandle(e3);
    HANDLE gone = OpenEventA(EVENT_ALL_ACCESS, FALSE, "W16.Event");
    printf("after the last close: %d err %lu\n", gone != NULL, GetLastError());
    gone = OpenEventA(EVENT_ALL_ACCESS, FALSE, "W16.Nothing");
    printf("never made: %d err %lu\n", gone != NULL, GetLastError());

    HANDLE s = CreateSemaphoreA(NULL, 2, 5, "W16.Sem");
    printf("semaphore: %d err %lu\n", s != NULL, GetLastError());
    HANDLE s2 = OpenSemaphoreA(SEMAPHORE_ALL_ACCESS, FALSE, "W16.Sem");
    printf("open: %d, takes: %lu %lu %lu\n", s2 != NULL, WaitForSingleObject(s2, 0), WaitForSingleObject(s, 0), WaitForSingleObject(s2, 0));
    LONG prev = -1;
    ReleaseSemaphore(s, 1, &prev);
    printf("previous %ld, now free: %lu\n", prev, WaitForSingleObject(s2, 0));

    /* A name belongs to one kind of object. */
    HANDLE wrong = CreateEventA(NULL, TRUE, FALSE, "W16.Sem");
    printf("event over a semaphore's name: %d err %lu\n", wrong != NULL, GetLastError());
    wrong = OpenMutexA(MUTEX_ALL_ACCESS, FALSE, "W16.Sem");
    printf("open as a mutex: %d err %lu\n", wrong != NULL, GetLastError());
    CloseHandle(s);
    CloseHandle(s2);

    HANDLE m = CreateMutexA(NULL, FALSE, "W16.Mutex");
    printf("mutex: %d err %lu\n", m != NULL, GetLastError());
    HANDLE m2 = CreateMutexA(NULL, FALSE, "W16.Mutex");
    printf("again: %d err %lu\n", m2 != NULL, GetLastError());
    printf("take: %lu, recursively: %lu\n", WaitForSingleObject(m, 0), WaitForSingleObject(m2, 0));
    printf("release: %d %d\n", ReleaseMutex(m), ReleaseMutex(m));
    BOOL released = ReleaseMutex(m);
    printf("once more, not owned: %d err %lu\n", released, GetLastError());
    CloseHandle(m);
    CloseHandle(m2);
}

/* -----| address waits |----- */

static volatile LONG word;
static volatile short half;
static volatile LONG woken;

static DWORD WINAPI waiter(void *arg)
{
    LONG undesired = 0;
    while (word == undesired)
        WaitOnAddress(&word, &undesired, sizeof word, INFINITE);
    InterlockedIncrement(&woken);
    return 0;
}

static void address(void)
{
    printf("--- WaitOnAddress\n");
    LONG same = 5, other = 6;
    word = 5;
    SetLastError(0);
    BOOL r = WaitOnAddress(&word, &same, sizeof word, 20);
    printf("same value, timed out: %d err %lu\n", r, GetLastError());
    r = WaitOnAddress(&word, &other, sizeof word, INFINITE);
    printf("different value, at once: %d\n", r);
    short s = 3;
    half = 3;
    r = WaitOnAddress(&half, &s, sizeof half, 5);
    printf("two bytes: %d err %lu\n", r, GetLastError());

    word = 0;
    HANDLE t[3];
    for (int i = 0; i < 3; i++) t[i] = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    Sleep(50);
    printf("still waiting: %ld\n", woken);
    word = 1;
    WakeByAddressAll((void *)&word);
    DWORD joined = WaitForMultipleObjects(3, t, TRUE, 5000);
    printf("joined: %lu, woken %ld\n", joined, woken);
    for (int i = 0; i < 3; i++) CloseHandle(t[i]);

    word = 0;
    woken = 0;
    HANDLE one = CreateThread(NULL, 0, waiter, NULL, 0, NULL);
    Sleep(20);
    word = 1;
    WakeByAddressSingle((void *)&word);
    printf("single: %lu\n", WaitForSingleObject(one, 5000));
    CloseHandle(one);
}

/* -----| fibers |----- */

static void *main_fiber, *fa, *fb;
static int trail[16], trail_n;

static void step(int v)
{
    trail[trail_n++] = v;
}

static void WINAPI fiber_a(void *param)
{
    step((int)(size_t)param);
    step(GetFiberData() == param);
    step(GetCurrentFiber() == fa);
    SwitchToFiber(fb);
    step(3);
    SwitchToFiber(main_fiber);
    step(99); /* never */
}

/* Registers a caller relies on survive a switch. */
static void WINAPI fiber_b(void *param)
{
    step(2);
    double d = 1.5;
    SwitchToFiber(fa);
    step((int)(d * 2));
    SwitchToFiber(main_fiber);
}

static int recurse(int n)
{
    volatile char pad[256];
    pad[0] = (char)n;
    return n == 0 ? pad[0] : recurse(n - 1) + 1;
}

static void WINAPI fiber_deep(void *param)
{
    *(int *)param = recurse(1000);
    __try {
        *(volatile int *)8 = 1;
        *(int *)param = -1;
    } __except (GetExceptionCode() == EXCEPTION_ACCESS_VIOLATION) {
        *((int *)param + 1) = 77;
    }
    SwitchToFiber(main_fiber);
}

static void fibers(void)
{
    printf("--- fibers\n");
    printf("a fiber yet: %d\n", IsThreadAFiber());
    main_fiber = ConvertThreadToFiber((void *)0x1234);
    printf("converted: %d, is one: %d, data %p\n", main_fiber != NULL, IsThreadAFiber(), GetFiberData());
    printf("twice: %d err %lu\n", ConvertThreadToFiber(NULL) != NULL, GetLastError());
    fa = CreateFiber(0, fiber_a, (void *)1);
    fb = CreateFiber(0, fiber_b, (void *)2);
    SwitchToFiber(fa);
    printf("back in main, current is main: %d\n", GetCurrentFiber() == main_fiber);
    SwitchToFiber(fb);
    printf("trail:");
    for (int i = 0; i < trail_n; i++) printf(" %d", trail[i]);
    printf("\n");
    DeleteFiber(fa);
    DeleteFiber(fb);

    int results[2] = { 0, 0 };
    void *deep = CreateFiberEx(64 * 1024, 1 << 20, 0, fiber_deep, results);
    SwitchToFiber(deep);
    printf("deep calls %d, caught fault %d\n", results[0], results[1]);
    DeleteFiber(deep);
    printf("to thread: %d, a fiber now: %d\n", ConvertFiberToThread(), IsThreadAFiber());
    printf("again: %d err %lu\n", ConvertFiberToThread(), GetLastError());
}

/* -----| named pipes |----- */

#define PIPE "\\\\.\\pipe\\w16pipe"

static DWORD WINAPI pipe_client(void *arg)
{
    Sleep(200); /* After the server is in ConnectNamedPipe: earlier gets 535, on Windows too. */
    HANDLE h = CreateFileA(PIPE, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) return GetLastError();
    char buf[32];
    DWORD n;
    WriteFile(h, "hello server", 12, &n, NULL);
    ReadFile(h, buf, sizeof buf, &n, NULL);
    /* Echo what came back, then hang up. */
    WriteFile(h, buf, n, &n, NULL);
    CloseHandle(h);
    return 0;
}

static void pipes(void)
{
    printf("--- named pipes\n");
    HANDLE none = CreateFileA(PIPE, GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("no server: %d err %lu\n", none != INVALID_HANDLE_VALUE, GetLastError());
    printf("wait for it: %d err %lu\n", WaitNamedPipeA(PIPE, 1), GetLastError());
    HANDLE s = CreateNamedPipeA(PIPE, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, NULL);
    printf("server: %d, type %lu\n", s != INVALID_HANDLE_VALUE, GetFileType(s));
    printf("free now: %d\n", WaitNamedPipeA(PIPE, 10));
    HANDLE t = CreateThread(NULL, 0, pipe_client, NULL, 0, NULL);
    BOOL c = ConnectNamedPipe(s, NULL);
    printf("connect: %d err %lu\n", c, c ? 0 : GetLastError());
    char buf[32];
    DWORD n = 0, avail = 0;
    memset(buf, 0, sizeof buf);
    ReadFile(s, buf, sizeof buf - 1, &n, NULL);
    printf("server got %lu: %s\n", n, buf);
    WriteFile(s, "reply", 5, &n, NULL);
    while (PeekNamedPipe(s, NULL, 0, NULL, &avail, NULL) && avail == 0) Sleep(1);
    printf("waiting to be read: %lu\n", avail);
    memset(buf, 0, sizeof buf);
    ReadFile(s, buf, sizeof buf - 1, &n, NULL);
    printf("echoed %lu: %s\n", n, buf);
    WaitForSingleObject(t, 5000);
    DWORD code = 99;
    GetExitCodeThread(t, &code);
    printf("client exit %lu\n", code);
    CloseHandle(t);
    BOOL r = ReadFile(s, buf, 8, &n, NULL);
    printf("after it hung up: %d err %lu\n", r, GetLastError());
    printf("disconnect: %d\n", DisconnectNamedPipe(s));

    /* A disconnected instance takes nobody until it connects again. */
    none = CreateFileA(PIPE, GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("disconnected: %d err %lu\n", none != INVALID_HANDLE_VALUE, GetLastError());
    CloseHandle(s);

    /* Connected already: a client that came first. */
    s = CreateNamedPipeA(PIPE, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, NULL);
    t = CreateThread(NULL, 0, pipe_client, NULL, 0, NULL);
    while (PeekNamedPipe(s, NULL, 0, NULL, &avail, NULL) == 0 || avail == 0) Sleep(1);
    c = ConnectNamedPipe(s, NULL);
    printf("connected first: %d err %lu\n", c, c ? 0 : GetLastError());
    /* And a second client, while one is using the only instance. */
    none = CreateFileA(PIPE, GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("busy: %d err %lu\n", none != INVALID_HANDLE_VALUE, GetLastError());
    ReadFile(s, buf, sizeof buf, &n, NULL);
    WriteFile(s, "x", 1, &n, NULL);
    ReadFile(s, buf, sizeof buf, &n, NULL);
    WaitForSingleObject(t, 5000);
    CloseHandle(t);
    CloseHandle(s);
    none = CreateFileA(PIPE, GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("server gone: %d err %lu\n", none != INVALID_HANDLE_VALUE, GetLastError());
}

/* -----| nameless pipes, as Rust's std makes them |----- */

typedef NTSTATUS(NTAPI *NtCreateNamedPipeFile_t)(PHANDLE, ULONG, POBJECT_ATTRIBUTES, PIO_STATUS_BLOCK, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, PLARGE_INTEGER);
typedef NTSTATUS(NTAPI *NtOpenFile_t)(PHANDLE, ACCESS_MASK, POBJECT_ATTRIBUTES, PIO_STATUS_BLOCK, ULONG, ULONG);

static void nameless(void)
{
    printf("--- nameless pipes\n");
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    NtCreateNamedPipeFile_t create = (NtCreateNamedPipeFile_t)GetProcAddress(nt, "NtCreateNamedPipeFile");
    NtOpenFile_t open = (NtOpenFile_t)GetProcAddress(nt, "NtOpenFile");
    printf("exports: %d %d\n", create != NULL, open != NULL);
    static WCHAR folder_name[] = L"\\Device\\NamedPipe\\";
    UNICODE_STRING us;
    us.Buffer = folder_name;
    us.Length = (USHORT)(wcslen(folder_name) * 2);
    us.MaximumLength = us.Length + 2;
    OBJECT_ATTRIBUTES oa;
    InitializeObjectAttributes(&oa, &us, 0, NULL, NULL);
    IO_STATUS_BLOCK io;
    HANDLE dir = NULL;
    NTSTATUS st = open(&dir, GENERIC_READ | SYNCHRONIZE, &oa, &io, FILE_SHARE_READ | FILE_SHARE_WRITE, 0);
    printf("folder: %08lx\n", (unsigned long)st);
    UNICODE_STRING empty;
    empty.Buffer = NULL;
    empty.Length = empty.MaximumLength = 0;
    InitializeObjectAttributes(&oa, &empty, 0, dir, NULL);
    LARGE_INTEGER timeout;
    timeout.QuadPart = -50 * 10000000LL;
    HANDLE server = NULL;
    st = create(&server, GENERIC_READ | GENERIC_WRITE | SYNCHRONIZE | FILE_READ_ATTRIBUTES, &oa, &io, FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_CREATE, 0x20 /* synchronous */, 0, 0, 0, 1, 4096, 4096, &timeout);
    printf("server: %08lx\n", (unsigned long)st);
    InitializeObjectAttributes(&oa, &empty, 0, server, NULL);
    HANDLE client = NULL;
    st = open(&client, GENERIC_READ | GENERIC_WRITE | SYNCHRONIZE, &oa, &io, FILE_SHARE_READ | FILE_SHARE_WRITE, 0x20);
    printf("client: %08lx\n", (unsigned long)st);
    DWORD n = 0;
    char buf[16];
    memset(buf, 0, sizeof buf);
    WriteFile(client, "to server", 9, &n, NULL);
    ReadFile(server, buf, sizeof buf - 1, &n, NULL);
    printf("server read %lu: %s\n", n, buf);
    memset(buf, 0, sizeof buf);
    WriteFile(server, "to client", 9, &n, NULL);
    ReadFile(client, buf, sizeof buf - 1, &n, NULL);
    printf("client read %lu: %s\n", n, buf);
    CloseHandle(client);
    BOOL r = ReadFile(server, buf, 4, &n, NULL);
    printf("client gone: %d err %lu\n", r, GetLastError());
    CloseHandle(server);
    CloseHandle(dir);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    named();
    address();
    fibers();
    pipes();
    nameless();
    printf("done\n");
    return 6;
}
