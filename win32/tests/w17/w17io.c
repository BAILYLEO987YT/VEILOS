/* W17 (S2): overlapped pipes that really wait, completion ports, final paths,
 * and the small system answers the Rust toolchain asks for. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define DUPLICATE_SAME_ATTRIBUTES 4 /* winnt.h leaves it out */
#define PIPE "\\\\.\\pipe\\w17pipe"

static HANDLE server, client;

static void make_pipes(void)
{
    server = CreateNamedPipeA(PIPE, PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, NULL);
    client = CreateFileA(PIPE, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
}

static DWORD WINAPI late_writer(void *arg)
{
    DWORD n;
    Sleep(100);
    WriteFile(client, "late data", 9, &n, NULL);
    return 0;
}

static void overlapped_pipe(void)
{
    printf("--- overlapped pipes\n");
    make_pipes();
    char buf[32];
    DWORD n = 77;
    OVERLAPPED ov;
    memset(&ov, 0, sizeof ov);
    ov.hEvent = CreateEventA(NULL, TRUE, FALSE, NULL);

    /* Nothing yet: pending, and asking doesn't block. */
    memset(buf, 0, sizeof buf);
    BOOL r = ReadFile(server, buf, sizeof buf - 1, NULL, &ov);
    DWORD e = GetLastError();
    printf("read: %d err %lu\n", r, e);
    r = GetOverlappedResult(server, &ov, &n, FALSE);
    e = GetLastError();
    printf("not done: %d err %lu, event %lu\n", r, e, WaitForSingleObject(ov.hEvent, 0));
    HANDLE t = CreateThread(NULL, 0, late_writer, NULL, 0, NULL);
    printf("event once written: %lu\n", WaitForSingleObject(ov.hEvent, 5000));
    r = GetOverlappedResult(server, &ov, &n, FALSE);
    printf("result: %d n=%lu %s\n", r, n, buf);
    WaitForSingleObject(t, 5000);
    CloseHandle(t);

    /* Something there already: done at once. */
    WriteFile(client, "ready", 5, &n, NULL);
    Sleep(20);
    ResetEvent(ov.hEvent);
    memset(buf, 0, sizeof buf);
    n = 0;
    r = ReadFile(server, buf, sizeof buf - 1, &n, &ov);
    printf("ready: %d n=%lu %s, event %lu\n", r, n, buf, WaitForSingleObject(ov.hEvent, 0));

    /* Waiting for the result. */
    ResetEvent(ov.hEvent);
    r = ReadFile(server, buf, sizeof buf - 1, NULL, &ov);
    e = GetLastError();
    printf("read: %d err %lu\n", r, e);
    t = CreateThread(NULL, 0, late_writer, NULL, 0, NULL);
    memset(buf, 0, sizeof buf);
    r = GetOverlappedResult(server, &ov, &n, TRUE);
    printf("waited: %d n=%lu\n", r, n);
    WaitForSingleObject(t, 5000);
    CloseHandle(t);

    /* Cancelled. */
    ResetEvent(ov.hEvent);
    r = ReadFile(server, buf, sizeof buf - 1, NULL, &ov);
    e = GetLastError();
    printf("read: %d err %lu\n", r, e);
    printf("cancel: %d\n", CancelIo(server));
    r = GetOverlappedResult(server, &ov, &n, TRUE);
    e = GetLastError();
    printf("cancelled: %d err %lu n=%lu\n", r, e, n);
    r = CancelIoEx(server, &ov);
    e = GetLastError();
    printf("nothing to cancel: %d err %lu\n", r, e);

    /* The other end goes while one waits. */
    ResetEvent(ov.hEvent);
    r = ReadFile(server, buf, sizeof buf - 1, NULL, &ov);
    e = GetLastError();
    printf("read: %d err %lu\n", r, e);
    CloseHandle(client);
    r = GetOverlappedResult(server, &ov, &n, TRUE);
    e = GetLastError();
    printf("hung up: %d err %lu\n", r, e);
    CloseHandle(server);
    CloseHandle(ov.hEvent);
}

static void ports(void)
{
    printf("--- completion ports\n");
    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
    printf("port: %d\n", port != NULL);
    DWORD n = 77;
    ULONG_PTR key = 77;
    OVERLAPPED *got = (OVERLAPPED *)1;
    BOOL r = GetQueuedCompletionStatus(port, &n, &key, &got, 10);
    DWORD e = GetLastError();
    printf("empty: %d err %lu, overlapped null %d\n", r, e, got == NULL);

    OVERLAPPED posted;
    printf("post: %d\n", PostQueuedCompletionStatus(port, 123, 456, &posted));
    r = GetQueuedCompletionStatus(port, &n, &key, &got, 1000);
    printf("posted: %d n=%lu key=%llu same %d\n", r, n, (unsigned long long)key, got == &posted);

    /* A file: a read done at once still sends a packet. */
    HANDLE f = CreateFileA("w17tmp.txt", GENERIC_READ | GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_FLAG_OVERLAPPED | FILE_FLAG_DELETE_ON_CLOSE, NULL);
    printf("bind: %d\n", CreateIoCompletionPort(f, port, 11, 0) == port);
    OVERLAPPED ov;
    memset(&ov, 0, sizeof ov);
    DWORD written = 0;
    r = WriteFile(f, "file data", 9, &written, &ov);
    if (!r && GetLastError() == ERROR_IO_PENDING) r = 1;
    printf("write: %d\n", r);
    r = GetQueuedCompletionStatus(port, &n, &key, &got, 2000);
    printf("packet: %d n=%lu key=%llu same %d\n", r, n, (unsigned long long)key, got == &ov);
    CloseHandle(f);

    /* A pipe: the packet comes when the read completes. */
    make_pipes();
    printf("bind: %d\n", CreateIoCompletionPort(server, port, 22, 0) == port);
    char buf[32];
    memset(buf, 0, sizeof buf);
    memset(&ov, 0, sizeof ov);
    r = ReadFile(server, buf, sizeof buf - 1, NULL, &ov);
    e = GetLastError();
    printf("read: %d err %lu\n", r, e);
    r = GetQueuedCompletionStatus(port, &n, &key, &got, 10);
    e = GetLastError();
    printf("nothing yet: %d err %lu\n", r, e);
    HANDLE t = CreateThread(NULL, 0, late_writer, NULL, 0, NULL);
    r = GetQueuedCompletionStatus(port, &n, &key, &got, 5000);
    printf("packet: %d n=%lu key=%llu same %d %s\n", r, n, (unsigned long long)key, got == &ov, buf);
    WaitForSingleObject(t, 5000);
    CloseHandle(t);

    /* Several at once, with the Ex form. */
    PostQueuedCompletionStatus(port, 1, 101, NULL);
    PostQueuedCompletionStatus(port, 2, 102, NULL);
    OVERLAPPED_ENTRY entries[4];
    ULONG removed = 0;
    r = GetQueuedCompletionStatusEx(port, entries, 4, &removed, 1000, FALSE);
    printf("ex: %d removed %lu:", r, removed);
    for (ULONG i = 0; i < removed; i++) printf(" %llu/%lu", (unsigned long long)entries[i].lpCompletionKey, entries[i].dwNumberOfBytesTransferred);
    printf("\n");
    r = GetQueuedCompletionStatusEx(port, entries, 4, &removed, 5, FALSE);
    e = GetLastError();
    printf("ex empty: %d err %lu removed %lu\n", r, e, removed);
    CloseHandle(client);
    CloseHandle(server);
    CloseHandle(port);
}

static void paths(void)
{
    printf("--- final paths\n");
    CreateDirectoryA("w17dir", NULL);
    HANDLE f = CreateFileA("w17dir\\final.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    WCHAR out[512], cwd[512];
    GetCurrentDirectoryW(512, cwd);
    DWORD n = GetFinalPathNameByHandleW(f, out, 512, 0);
    size_t skip = wcslen(cwd);
    if (skip > 0 && cwd[skip - 1] == L'\\') skip--;
    /* What's past the current folder, which differs between machines. */
    printf("prefix \\\\?\\ %d, length matches %d, tail ", wcsncmp(out, L"\\\\?\\", 4) == 0, n == wcslen(out));
    for (const WCHAR *p = out + 4 + skip; *p; p++) putchar((char)*p);
    printf("\n");
    WCHAR tiny[4];
    n = GetFinalPathNameByHandleW(f, tiny, 4, 0);
    printf("too small asks for more: %d\n", n > 4);
    /* A handle by itself says where it is, even for a folder. */
    CloseHandle(f);
    HANDLE d = CreateFileA("w17dir", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    n = GetFinalPathNameByHandleW(d, out, 512, 0);
    printf("folder, tail ");
    for (const WCHAR *p = out + 4 + skip; *p; p++) putchar((char)*p);
    printf("\n");
    CloseHandle(d);
    DeleteFileA("w17dir\\final.txt");
    RemoveDirectoryA("w17dir");

    /* Two opens of a file are the same file; two files aren't. */
    HANDLE a = CreateFileA("w17a.txt", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
    HANDLE a2 = CreateFileA("w17a.txt", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    HANDLE b = CreateFileA("w17b.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
    BY_HANDLE_FILE_INFORMATION ia, ia2, ib;
    GetFileInformationByHandle(a, &ia);
    GetFileInformationByHandle(a2, &ia2);
    GetFileInformationByHandle(b, &ib);
    printf("same file %d, other file %d\n",
        ia.nFileIndexLow == ia2.nFileIndexLow && ia.nFileIndexHigh == ia2.nFileIndexHigh,
        ia.nFileIndexLow == ib.nFileIndexLow && ia.nFileIndexHigh == ib.nFileIndexHigh);
    CloseHandle(a);
    CloseHandle(a2);
    CloseHandle(b);
}

/* The NUL device, as Rust gives a child that has no stdin. */
static void nul_device(void)
{
    printf("--- NUL\n");
    const char *names[] = { "NUL", "nul", "\\\\.\\NUL" };
    for (int k = 0; k < 3; k++) {
        HANDLE h = CreateFileA(names[k], GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
        DWORD n = 77;
        char b[8];
        printf("%s: open %d, type %lu", names[k], h != INVALID_HANDLE_VALUE, GetFileType(h));
        BOOL w = WriteFile(h, "gone", 4, &n, NULL);
        printf(", write %d n=%lu", w, n);
        n = 77;
        BOOL r = ReadFile(h, b, 8, &n, NULL);
        printf(", read %d n=%lu\n", r, n);
        CloseHandle(h);
    }
}

/* cargo makes a port for every compiler it runs, so closing one has to give it
   back; and a file bound to a closed port must not take the next file's packets. */
static void ports_come_back(void)
{
    printf("--- ports released\n");
    int made = 0;
    for (int k = 0; k < 200; k++) {
        HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
        if (!port)
            break;
        made++;
        CloseHandle(port);
    }
    printf("made and closed %d\n", made);
    HANDLE port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, NULL, 0, 1);
    DWORD n = 0;
    ULONG_PTR key = 0;
    OVERLAPPED *o = NULL;
    BOOL got = GetQueuedCompletionStatus(port, &n, &key, &o, 0);
    printf("empty port: %d error %lu\n", got, GetLastError());
    PostQueuedCompletionStatus(port, 5, 77, NULL);
    got = GetQueuedCompletionStatus(port, &n, &key, &o, 0);
    printf("posted: %d n=%lu key=%d\n", got, n, (int)key);
    CloseHandle(port);
}

/* A thread already running when a DLL with TLS is loaded gets its own copy too. */
typedef int(__cdecl *TlsGet)(void);
typedef void(__cdecl *TlsSet)(int);
static TlsGet tls_get;
static TlsSet tls_set;
static HANDLE tls_go;
static int tls_seen[2];
static DWORD WINAPI early_thread(LPVOID unused)
{
    (void)unused;
    WaitForSingleObject(tls_go, INFINITE);
    tls_seen[0] = tls_get();
    tls_set(tls_seen[0] + 1);
    tls_seen[1] = tls_get();
    return 0;
}
static DWORD WINAPI late_thread(LPVOID unused)
{
    (void)unused;
    tls_seen[0] = tls_get();
    tls_set(100);
    tls_seen[1] = tls_get();
    return 0;
}

/* rustc loads a proc macro by its full path, from a folder nothing searches. */
static void dll_by_path(void)
{
    printf("--- LoadLibrary with a path\n");
    char path[MAX_PATH];
    DWORD n = GetModuleFileNameA(NULL, path, MAX_PATH);
    while (n > 0 && path[n - 1] != '\\')
        n--;
    path[n] = 0;
    char full[MAX_PATH + 32], prefixed[MAX_PATH + 32];
    sprintf(full, "%sw17sub\\w17dll.dll", path);
    sprintf(prefixed, "\\\\?\\%s", full);
    tls_go = CreateEventA(NULL, TRUE, FALSE, NULL);
    HANDLE early = CreateThread(NULL, 0, early_thread, NULL, 0, NULL);
    typedef int(__cdecl * Answer)(void);
    HMODULE bare = LoadLibraryA("w17dll.dll");
    printf("by name alone: %d error %lu\n", bare != NULL, GetLastError());
    HMODULE m = LoadLibraryA(full);
    printf("by path: %d\n", m != NULL);
    Answer a = m ? (Answer)GetProcAddress(m, "w17_answer") : NULL;
    printf("export: %d\n", a ? a() : -1);
    int *data = m ? (int *)GetProcAddress(m, "w17_data") : NULL;
    printf("data export: %d\n", data ? *data : -1);
    int *attaches = m ? (int *)GetProcAddress(m, "w17_callback_attaches") : NULL;
    printf("TLS callback process attaches: %d\n", attaches ? *attaches : -1);
    tls_get = m ? (TlsGet)GetProcAddress(m, "w17_tls_get") : NULL;
    tls_set = m ? (TlsSet)GetProcAddress(m, "w17_tls_set") : NULL;
    if (tls_get && tls_set) {
        printf("main thread TLS: %d\n", tls_get());
        tls_set(9);
        SetEvent(tls_go);
        WaitForSingleObject(early, INFINITE);
        printf("thread already running: %d then %d\n", tls_seen[0], tls_seen[1]);
        HANDLE late = CreateThread(NULL, 0, late_thread, NULL, 0, NULL);
        WaitForSingleObject(late, INFINITE);
        CloseHandle(late);
        printf("thread started later: %d then %d\n", tls_seen[0], tls_seen[1]);
        printf("main thread still: %d\n", tls_get());
    }
    CloseHandle(early);
    CloseHandle(tls_go);
    HMODULE again = LoadLibraryA(prefixed);
    printf("by long path prefix, same module: %d\n", again == m);
    HMODULE named = LoadLibraryA("w17dll.dll");
    printf("by name once loaded, same module: %d\n", named == m);
    HMODULE missing = LoadLibraryA("C:\\no\\such\\folder\\w17none.dll");
    printf("missing path: %d error %lu\n", missing != NULL, GetLastError());
    if (named)
        FreeLibrary(named);
    if (again)
        FreeLibrary(again);
    if (m)
        FreeLibrary(m);
}

/* Handles go to children only when asked for. */
static int inheritable(HANDLE h)
{
    DWORD flags = 99;
    if (!GetHandleInformation(h, &flags))
        return -1;
    return (flags & HANDLE_FLAG_INHERIT) != 0;
}

static void inheritance(void)
{
    printf("--- which handles children get\n");
    SECURITY_ATTRIBUTES yes = {sizeof yes, NULL, TRUE}, no = {sizeof no, NULL, FALSE};
    HANDLE me = GetCurrentProcess(), r, w, h, d;
    CreatePipe(&r, &w, NULL, 0);
    printf("pipe, no attributes: %d %d\n", inheritable(r), inheritable(w));
    CloseHandle(r);
    CloseHandle(w);
    CreatePipe(&r, &w, &yes, 0);
    printf("pipe, inherit: %d %d\n", inheritable(r), inheritable(w));
    CloseHandle(r);
    CloseHandle(w);
    h = CreateNamedPipeA("\\\\.\\pipe\\w17inh", PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 512, 512, 0, NULL);
    printf("named pipe, no attributes: %d\n", inheritable(h));
    d = CreateFileA("\\\\.\\pipe\\w17inh", GENERIC_READ, 0, &yes, OPEN_EXISTING, 0, NULL);
    printf("its client, inherit: %d\n", inheritable(d));
    CloseHandle(d);
    CloseHandle(h);
    h = CreateFileA("NUL", GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    printf("file, no attributes: %d\n", inheritable(h));
    DuplicateHandle(me, h, me, &d, 0, TRUE, DUPLICATE_SAME_ACCESS);
    printf("duplicate, inherit: %d\n", inheritable(d));
    CloseHandle(d);
    CloseHandle(h);
    h = CreateFileA("NUL", GENERIC_WRITE, 0, &yes, OPEN_EXISTING, 0, NULL);
    printf("file, inherit: %d\n", inheritable(h));
    DuplicateHandle(me, h, me, &d, 0, FALSE, DUPLICATE_SAME_ACCESS);
    printf("duplicate, no inherit: %d\n", inheritable(d));
    CloseHandle(d);
    DuplicateHandle(me, h, me, &d, 0, FALSE, DUPLICATE_SAME_ACCESS | DUPLICATE_SAME_ATTRIBUTES);
    printf("duplicate, same attributes: %d\n", inheritable(d));
    CloseHandle(d);
    CloseHandle(h);
    h = CreateFileA("NUL", GENERIC_WRITE, 0, &no, OPEN_EXISTING, 0, NULL);
    printf("file, inherit false: %d\n", inheritable(h));
    CloseHandle(h);
}

/* What a build script does to ask rustc a question: source down the child's
   stdin. The child reads to the end, so it must not hold the write end itself. */
static int read_all_of_stdin(void)
{
    char buf[256];
    DWORD n;
    int total = 0;
    while (ReadFile(GetStdHandle(STD_INPUT_HANDLE), buf, sizeof buf, &n, NULL) && n > 0)
        total += n;
    return total;
}

static void child_reads(HANDLE ours, HANDLE theirs)
{
    HANDLE me = GetCurrentProcess(), out;
    DuplicateHandle(me, GetStdHandle(STD_OUTPUT_HANDLE), me, &out, 0, TRUE, DUPLICATE_SAME_ACCESS);
    char self[MAX_PATH], line[MAX_PATH + 16];
    GetModuleFileNameA(NULL, self, MAX_PATH);
    sprintf(line, "\"%s\" stdin", self);
    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = theirs;
    si.hStdOutput = out;
    si.hStdError = out;
    BOOL made = CreateProcessA(self, line, NULL, NULL, TRUE, 0, NULL, NULL, &si, &pi);
    printf("started: %d\n", made);
    CloseHandle(theirs);
    CloseHandle(out);
    DWORD n = 0;
    WriteFile(ours, "twelve bytes", 12, &n, NULL);
    CloseHandle(ours);
    DWORD waited = WaitForSingleObject(pi.hProcess, 20000);
    printf("finished: %d\n", waited == WAIT_OBJECT_0);
    if (waited != WAIT_OBJECT_0)
        TerminateProcess(pi.hProcess, 99);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    printf("bytes it read: %lu\n", code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
}

static void stdin_to_the_end(void)
{
    printf("--- child reads stdin to the end\n");
    SECURITY_ATTRIBUTES yes = {sizeof yes, NULL, TRUE};
    const char *name = "\\\\.\\pipe\\w17stdin";
    HANDLE ours = CreateNamedPipeA(name, PIPE_ACCESS_OUTBOUND, PIPE_TYPE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, NULL);
    HANDLE theirs = CreateFileA(name, GENERIC_READ, 0, &yes, OPEN_EXISTING, 0, NULL);
    child_reads(ours, theirs);
}

/* The same, with the pipe made as Rust's standard library makes it. */
typedef struct {
    USHORT Length, MaximumLength;
    PWSTR Buffer;
} W17_UNICODE;
typedef struct {
    ULONG Length;
    HANDLE RootDirectory;
    W17_UNICODE *ObjectName;
    ULONG Attributes;
    PVOID SecurityDescriptor, SecurityQualityOfService;
} W17_ATTRS;
typedef LONG(NTAPI *NtOpen)(HANDLE *, ULONG, W17_ATTRS *, ULONG_PTR *, ULONG, ULONG);
typedef LONG(NTAPI *NtPipe)(HANDLE *, ULONG, W17_ATTRS *, ULONG_PTR *, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, ULONG, LONGLONG *);

static void stdin_as_rust_does(void)
{
    printf("--- the same through the NT calls\n");
    HMODULE nt = GetModuleHandleA("ntdll.dll");
    NtOpen open = (NtOpen)GetProcAddress(nt, "NtOpenFile");
    NtPipe pipe = (NtPipe)GetProcAddress(nt, "NtCreateNamedPipeFile");
    ULONG_PTR iosb[2];
    wchar_t folder[] = L"\\Device\\NamedPipe\\";
    W17_UNICODE path = {sizeof folder - 2, sizeof folder, folder}, empty = {0, 0, NULL};
    W17_ATTRS a = {sizeof a, NULL, &path, 0, NULL, NULL};
    HANDLE fs = NULL, ours = NULL, theirs = NULL;
    LONG s = open(&fs, SYNCHRONIZE | GENERIC_READ, &a, iosb, FILE_SHARE_READ | FILE_SHARE_WRITE, 0x20);
    printf("folder: %ld\n", s);
    a.ObjectName = &empty;
    a.RootDirectory = fs;
    LONGLONG timeout = -500000;
    s = pipe(&ours, SYNCHRONIZE | GENERIC_WRITE, &a, iosb, FILE_SHARE_READ, 2, 0x20, 0, 0, 0, 1, 65536, 65536, &timeout);
    printf("ours: %ld inheritable %d\n", s, inheritable(ours));
    a.RootDirectory = ours;
    a.Attributes = 2; /* OBJ_INHERIT */
    s = open(&theirs, SYNCHRONIZE | GENERIC_READ, &a, iosb, 0, 0x40 | 0x20);
    printf("theirs: %ld inheritable %d\n", s, inheritable(theirs));
    child_reads(ours, theirs);
    CloseHandle(fs);
}

/* Sizes across the heap's free lists, freed out of order and reused: every block
   keeps its own bytes. */
static void heap_churn(void)
{
    printf("--- heap churn\n");
    enum { SLOTS = 400 };
    static unsigned char *block[SLOTS];
    static size_t size[SLOTS];
    HANDLE heap = GetProcessHeap();
    unsigned seed = 12345;
    int bad = 0, nulls = 0;
    for (int round = 0; round < 6000; round++) {
        seed = seed * 1103515245u + 12345u;
        int i = (seed >> 8) % SLOTS;
        if (block[i]) {
            for (size_t k = 0; k < size[i]; k += 97)
                if (block[i][k] != (unsigned char)i)
                    bad++;
            if (block[i][size[i] - 1] != (unsigned char)i)
                bad++;
            HeapFree(heap, 0, block[i]);
            block[i] = NULL;
        } else {
            seed = seed * 1103515245u + 12345u;
            /* Mostly above 1 KiB, up to about 600 KiB. */
            size_t n = 16u << ((seed >> 12) % 16);
            n += (seed >> 16) % n;
            block[i] = HeapAlloc(heap, 0, n);
            if (!block[i]) {
                nulls++;
                continue;
            }
            size[i] = n;
            memset(block[i], i, n);
        }
    }
    for (int i = 0; i < SLOTS; i++)
        if (block[i])
            HeapFree(heap, 0, block[i]);
    printf("wrong bytes: %d, failed: %d\n", bad, nulls);
}

typedef BOOL(WINAPI *Prng)(PUCHAR, SIZE_T);

static void answers(void)
{
    printf("--- small answers\n");
    int x;
    printf("pointers round trip: %d\n", DecodePointer(EncodePointer(&x)) == &x);
    WORD types[6];
    GetStringTypeW(CT_CTYPE1, L"aZ5 .", 5, types);
    printf("types: %x %x %x %x %x\n", types[0] & 0x1FF, types[1] & 0x1FF, types[2] & 0x1FF, types[3] & 0x1FF, types[4] & 0x1FF);
    MEMORYSTATUSEX m;
    m.dwLength = sizeof m;
    BOOL r = GlobalMemoryStatusEx(&m);
    printf("memory: %d, sensible %d\n", r, m.ullTotalPhys > (1ull << 20) && m.ullAvailPhys <= m.ullTotalPhys);
    HMODULE p = LoadLibraryA("bcryptprimitives.dll");
    Prng prng = p ? (Prng)GetProcAddress(p, "ProcessPrng") : NULL;
    unsigned char a[32], c[32];
    memset(a, 0, 32);
    memset(c, 0, 32);
    printf("prng: %d", prng != NULL);
    if (prng) {
        printf(" %d %d", prng(a, 32), prng(c, 32));
        printf(" differ %d, not zero %d", memcmp(a, c, 32) != 0, memcmp(a, "\0\0\0\0\0\0\0\0", 8) != 0);
    }
    printf("\n");
    HANDLE job = CreateJobObjectW(NULL, NULL);
    printf("job: %d, assign self %d\n", job != NULL, AssignProcessToJobObject(job, GetCurrentProcess()));
    CloseHandle(job);
    HANDLE f = CreateFileA("w17lock.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_FLAG_DELETE_ON_CLOSE, NULL);
    OVERLAPPED ov;
    memset(&ov, 0, sizeof ov);
    printf("lock: %d", LockFileEx(f, LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, &ov));
    printf(", unlock: %d\n", UnlockFileEx(f, 0, 1, 0, &ov));
    HANDLE again = ReOpenFile(f, GENERIC_WRITE, 0, 0);
    printf("reopen shared-none is refused or a handle: %d\n", again == INVALID_HANDLE_VALUE || again != NULL);
    if (again != INVALID_HANDLE_VALUE) CloseHandle(again);
    CloseHandle(f);
    ULONG guarantee = 16 * 1024;
    printf("stack guarantee: %d\n", SetThreadStackGuarantee(&guarantee));
}

/* What LLD does with its output: a temp file, sized, mapped, the mapping closed,
 * written through the view, then renamed. */
static void mapped_output(void)
{
    printf("--- mapped output\n");
    HANDLE f = CreateFileA("w17out.tmp", GENERIC_READ | GENERIC_WRITE | DELETE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, CREATE_ALWAYS, 0, NULL);
    LARGE_INTEGER size;
    size.QuadPart = 300000;
    SetFilePointerEx(f, size, NULL, FILE_BEGIN);
    printf("size: %d\n", SetEndOfFile(f));
    size.QuadPart = 0;
    SetFilePointerEx(f, size, NULL, FILE_BEGIN);
    HANDLE m = CreateFileMappingA(f, NULL, PAGE_READWRITE, 0, 300000, NULL);
    unsigned char *v = (unsigned char *)MapViewOfFile(m, FILE_MAP_WRITE, 0, 0, 300000);
    printf("mapped: %d %d\n", m != NULL, v != NULL);
    printf("mapping closed: %d\n", CloseHandle(m));
    for (int i = 0; i < 300000; i += 4096) v[i] = (unsigned char)(i / 4096 + 1);
    v[299999] = 0x5A;
    printf("flush: %d, unmap: %d\n", FlushViewOfFile(v, 0), UnmapViewOfFile(v));
    FILE_RENAME_INFO *ri = (FILE_RENAME_INFO *)LocalAlloc(LPTR, sizeof(FILE_RENAME_INFO) + 1024);
    WCHAR full[256];
    GetFullPathNameW(L"w17out.bin", 256, full, NULL);
    ri->ReplaceIfExists = TRUE;
    ri->FileNameLength = (DWORD)(wcslen(full) * 2);
    memcpy(ri->FileName, full, ri->FileNameLength);
    printf("rename: %d\n", SetFileInformationByHandle(f, FileRenameInfo, ri, (DWORD)(sizeof(FILE_RENAME_INFO) + ri->FileNameLength)));
    CloseHandle(f);
    f = CreateFileA("w17out.bin", GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_FLAG_DELETE_ON_CLOSE * 0, NULL);
    LARGE_INTEGER got;
    GetFileSizeEx(f, &got);
    unsigned char b[4097];
    DWORD n;
    ReadFile(f, b, 4097, &n, NULL);
    printf("size %lld, bytes %u %u\n", got.QuadPart, b[0], b[4096]);
    size.QuadPart = 299999;
    SetFilePointerEx(f, size, NULL, FILE_BEGIN);
    ReadFile(f, b, 1, &n, NULL);
    printf("last byte %x\n", b[0]);
    CloseHandle(f);
    DeleteFileA("w17out.bin");
    printf("temp gone: %d\n", GetFileAttributesA("w17out.tmp") == INVALID_FILE_ATTRIBUTES);
}

#include <wincrypt.h>

/* CryptoAPI hashing, as libgit2 uses it. Short inputs, and one past a block. */
static void hashes(void)
{
    printf("--- hashes\n");
    HCRYPTPROV prov = 0;
    printf("provider: %d\n", CryptAcquireContextA(&prov, NULL, MS_ENH_RSA_AES_PROV_A, PROV_RSA_AES, CRYPT_VERIFYCONTEXT));
    static char long_input[1000];
    memset(long_input, 'a', sizeof long_input);
    const struct { ALG_ID alg; const char *s; DWORD n; } cases[] = {
        { CALG_SHA1, "abc", 3 }, { CALG_SHA_256, "abc", 3 }, { CALG_SHA1, "", 0 }, { CALG_SHA_256, "", 0 },
        { CALG_SHA1, long_input, 1000 }, { CALG_SHA_256, long_input, 1000 },
    };
    for (int k = 0; k < 6; k++) {
        HCRYPTHASH h = 0;
        BOOL c = CryptCreateHash(prov, cases[k].alg, 0, 0, &h);
        /* In two pieces, so state carries between calls. */
        DWORD half = cases[k].n / 2;
        CryptHashData(h, (const BYTE *)cases[k].s, half, 0);
        CryptHashData(h, (const BYTE *)cases[k].s + half, cases[k].n - half, 0);
        DWORD size = 0, len = sizeof size;
        CryptGetHashParam(h, HP_HASHSIZE, (BYTE *)&size, &len, 0);
        BYTE out[32];
        len = sizeof out;
        BOOL g = CryptGetHashParam(h, HP_HASHVAL, out, &len, 0);
        printf("%d %d size %lu len %lu ", c, g, size, len);
        for (DWORD i = 0; i < len; i++) printf("%02x", out[i]);
        printf("\n");
        CryptDestroyHash(h);
    }
    printf("release: %d\n", CryptReleaseContext(prov, 0));
}

int main(int argc, char **argv)
{
    (void)argv;
    if (argc > 1)
        return read_all_of_stdin();
    setvbuf(stdout, NULL, _IONBF, 0);
    overlapped_pipe();
    ports();
    paths();
    mapped_output();
    hashes();
    nul_device();
    ports_come_back();
    dll_by_path();
    inheritance();
    stdin_to_the_end();
    stdin_as_rust_does();
    heap_churn();
    answers();
    printf("done\n");
    return 4;
}
