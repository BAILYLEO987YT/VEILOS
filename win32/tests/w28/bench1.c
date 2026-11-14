/* bench1: what Security and Performance Mode cost, in microseconds per
 * operation. Each line: name, operations, microseconds per operation.
 *
 *   bench1           every benchmark
 *   bench1 child     (started by the process benchmark: exits at once)
 *
 * The CPU loop is the control: no mode should change it.
 */
#include <winsock2.h>
#include <windows.h>
#include <stdio.h>
#include <string.h>

static LARGE_INTEGER freq;
static char here[MAX_PATH];

static double now_us(void) {
    LARGE_INTEGER t;
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart * 1e6 / (double)freq.QuadPart;
}

static const char *at(const char *name) {
    static char out[4][MAX_PATH];
    static int i;
    char *p = out[i++ & 3];
    snprintf(p, MAX_PATH, "%s%s", here, name);
    return p;
}

static void report(const char *name, int n, double t0) {
    printf("%-22s %7d  %10.2f us\n", name, n, (now_us() - t0) / n);
}

static volatile unsigned sink;

static void cpu(void) {
    int n = 20000000, i;
    unsigned x = 1;
    double t0 = now_us();
    for (i = 0; i < n; i++) x = x * 1664525u + 1013904223u;
    sink = x;
    report("cpu (control)", n / 1000, t0);
}

static void syscalls(void) {
    int n = 20000, i;
    double t0;
    HANDLE ev = CreateEventA(NULL, TRUE, FALSE, NULL);
    t0 = now_us();
    for (i = 0; i < n; i++) SetEvent(ev);
    report("SetEvent", n, t0);
    t0 = now_us();
    for (i = 0; i < n; i++) GetFileAttributesA(at("bench1.exe"));
    report("GetFileAttributes", n, t0);
    CloseHandle(ev);
}

static void files(void) {
    int n = 300, i;
    static char block[4096];
    DWORD done;
    double t0 = now_us();
    for (i = 0; i < n; i++) {
        HANDLE h = CreateFileA(at("bench1_tmp.bin"), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        WriteFile(h, block, sizeof block, &done, NULL);
        CloseHandle(h);
        DeleteFileA(at("bench1_tmp.bin"));
    }
    report("file create+4K+delete", n, t0);

    {
        static char big[1 << 20];
        HANDLE h = CreateFileA(at("bench1_big.bin"), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        WriteFile(h, big, sizeof big, &done, NULL);
        CloseHandle(h);
        n = 20;
        t0 = now_us();
        for (i = 0; i < n; i++) {
            h = CreateFileA(at("bench1_big.bin"), GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
            while (ReadFile(h, big, 65536, &done, NULL) && done) {}
            CloseHandle(h);
        }
        report("read 1 MB", n, t0);
        DeleteFileA(at("bench1_big.bin"));
    }
}

static void memory(void) {
    int n = 2000, i;
    double t0 = now_us();
    for (i = 0; i < n; i++) {
        char *p = (char *)VirtualAlloc(NULL, 65536, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        int k;
        for (k = 0; k < 65536; k += 4096) p[k] = 1;
        VirtualFree(p, 0, MEM_RELEASE);
    }
    report("alloc+touch+free 64K", n, t0);
}

static HANDLE ping, pong;
static DWORD WINAPI ponger(LPVOID n) {
    int i;
    for (i = 0; i < (int)(INT_PTR)n; i++) {
        WaitForSingleObject(ping, INFINITE);
        SetEvent(pong);
    }
    return 0;
}

static void threads(void) {
    int n = 2000, i;
    HANDLE t;
    double t0;
    ping = CreateEventA(NULL, FALSE, FALSE, NULL);
    pong = CreateEventA(NULL, FALSE, FALSE, NULL);
    t = CreateThread(NULL, 0, ponger, (LPVOID)(INT_PTR)n, 0, NULL);
    t0 = now_us();
    for (i = 0; i < n; i++) {
        SetEvent(ping);
        WaitForSingleObject(pong, INFINITE);
    }
    report("thread ping-pong", n, t0);
    WaitForSingleObject(t, INFINITE);
    CloseHandle(t);
}

static void processes(const char *self) {
    int n = 10, i;
    char cmd[300];
    double t0;
    snprintf(cmd, sizeof cmd, "\"%s\" child", self);
    t0 = now_us();
    for (i = 0; i < n; i++) {
        STARTUPINFOA si;
        PROCESS_INFORMATION pi;
        memset(&si, 0, sizeof si);
        si.cb = sizeof si;
        if (!CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi)) {
            printf("CreateProcess failed (%lu)\n", GetLastError());
            return;
        }
        WaitForSingleObject(pi.hProcess, INFINITE);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    report("process start+exit", n, t0);
}

static void sockets(void) {
    WSADATA w;
    SOCKET l, c, s;
    struct sockaddr_in a;
    int len = sizeof a, n = 2000, i;
    char buf[1024] = {0};
    double t0;
    WSAStartup(MAKEWORD(2, 2), &w);
    l = socket(AF_INET, SOCK_STREAM, 0);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    bind(l, (struct sockaddr *)&a, sizeof a);
    listen(l, 1);
    getsockname(l, (struct sockaddr *)&a, &len);
    c = socket(AF_INET, SOCK_STREAM, 0);
    connect(c, (struct sockaddr *)&a, sizeof a);
    s = accept(l, NULL, NULL);
    t0 = now_us();
    for (i = 0; i < n; i++) {
        int got = 0;
        send(c, buf, sizeof buf, 0);
        while (got < (int)sizeof buf) got += recv(s, buf, sizeof buf - got, 0);
    }
    report("loopback 1K send+recv", n, t0);
    closesocket(s);
    closesocket(c);
    closesocket(l);
}

int main(int argc, char **argv) {
    char *slash;
    if (argc > 1 && !strcmp(argv[1], "child")) return 0;
    QueryPerformanceFrequency(&freq);
    GetModuleFileNameA(NULL, here, MAX_PATH);
    slash = strrchr(here, '\\');
    if (slash) slash[1] = 0; else here[0] = 0;
    cpu();
    syscalls();
    files();
    memory();
    threads();
    processes(argv[0]);
    sockets();
    printf("done\n");
    return 0;
}
