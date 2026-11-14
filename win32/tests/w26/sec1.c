/* sec1: Security Mode (src/security.rs) as Windows programs see it.
 *
 *   sec1            files, processes and sockets, printing what each call answered
 *   sec1 sleep      write this process's id to sec1.pid (beside it) and wait 60 s
 *   sec1 poke       read and end the process named in sec1.pid (not a relative)
 *   sec1 system     create and delete a file in \system (protected in Security Mode)
 *   sec1 wx         writable+executable memory (refused by Security Mode's W^X)
 *   sec1 child      wait 30 s (started by sec1 itself)
 *
 * On Windows every call here succeeds for a normal user (except the
 * connection to 10.0.2.2, which needs a server there). On VEILOS what's
 * allowed depends on the signed-in user's role: a guest gets
 * ERROR_ACCESS_DENIED (5) for writes, deletes and new processes, and
 * WSAEACCES (10013) beyond loopback; an admin may end an unrelated program
 * but not read its memory.
 */
#include <winsock2.h>
#include <windows.h>
#include <stdio.h>
#include <string.h>

/* A file beside this program. */
static char here[MAX_PATH];
static const char *at(const char *name) {
    static char out[4][MAX_PATH];
    static int i;
    char *p = out[i++ & 3];
    snprintf(p, MAX_PATH, "%s%s", here, name);
    return p;
}

static void show(const char *what, BOOL ok) {
    printf("%s: %s", what, ok ? "ok" : "failed");
    if (!ok) printf(" (error %lu)", GetLastError());
    printf("\n");
}

static int files(void) {
    HANDLE h;
    char buf[16];
    DWORD n = 0;
    h = CreateFileA(at("sec1.exe"), GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    show("open for reading", h != INVALID_HANDLE_VALUE);
    if (h != INVALID_HANDLE_VALUE) {
        show("read", ReadFile(h, buf, sizeof buf, &n, NULL) && n == sizeof buf);
        CloseHandle(h);
    }
    h = CreateFileA(at("sec1_new.txt"), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    show("create a file", h != INVALID_HANDLE_VALUE);
    if (h != INVALID_HANDLE_VALUE) {
        show("write", WriteFile(h, "hello", 5, &n, NULL));
        CloseHandle(h);
    }
    show("create a folder", CreateDirectoryA(at("sec1_dir"), NULL));
    show("rename", MoveFileA(at("sec1_new.txt"), at("sec1_moved.txt")));
    show("delete", DeleteFileA(at("sec1_moved.txt")));
    RemoveDirectoryA(at("sec1_dir"));
    return 0;
}

static int processes(const char *self) {
    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    char cmd[300];
    char buf[2];
    SIZE_T got = 0;
    BOOL ok;
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    snprintf(cmd, sizeof cmd, "\"%s\" child", self);
    ok = CreateProcessA(NULL, cmd, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    show("start a program", ok);
    if (!ok) return 0;
    show("read own child's memory", ReadProcessMemory(pi.hProcess, (LPCVOID)GetModuleHandleA(NULL), buf, 2, &got) && got == 2);
    show("end own child", TerminateProcess(pi.hProcess, 7));
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return 0;
}

static int sockets(void) {
    WSADATA w;
    SOCKET l, c;
    struct sockaddr_in a;
    int len = sizeof a;
    WSAStartup(MAKEWORD(2, 2), &w);
    l = socket(AF_INET, SOCK_STREAM, 0);
    memset(&a, 0, sizeof a);
    a.sin_family = AF_INET;
    a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    show("loopback bind", bind(l, (struct sockaddr *)&a, sizeof a) == 0);
    show("loopback listen", listen(l, 1) == 0);
    getsockname(l, (struct sockaddr *)&a, &len);
    c = socket(AF_INET, SOCK_STREAM, 0);
    show("loopback connect", connect(c, (struct sockaddr *)&a, sizeof a) == 0);
    closesocket(c);
    closesocket(l);

    /* Beyond this machine: 10.0.2.2 port 9 (nothing listens; a refusal or a
     * time-out is fine, WSAEACCES means Security Mode said no). */
    c = socket(AF_INET, SOCK_STREAM, 0);
    a.sin_addr.s_addr = inet_addr("10.0.2.2");
    a.sin_port = htons(9);
    {
        u_long nb = 1;
        int r;
        ioctlsocket(c, FIONBIO, &nb);
        r = connect(c, (struct sockaddr *)&a, sizeof a);
        printf("network connect: %s\n", r == 0 || WSAGetLastError() != WSAEACCES ? "allowed" : "denied (WSAEACCES)");
    }
    closesocket(c);
    l = socket(AF_INET, SOCK_STREAM, 0);
    a.sin_addr.s_addr = htonl(INADDR_ANY);
    a.sin_port = 0;
    bind(l, (struct sockaddr *)&a, sizeof a);
    printf("network listen: %s\n", listen(l, 1) == 0 ? "allowed" : WSAGetLastError() == WSAEACCES ? "denied (WSAEACCES)" : "failed");
    closesocket(l);
    return 0;
}

int main(int argc, char **argv) {
    char *slash;
    GetModuleFileNameA(NULL, here, MAX_PATH);
    slash = strrchr(here, '\\');
    if (slash) slash[1] = 0; else here[0] = 0;
    if (argc > 1 && !strcmp(argv[1], "child")) {
        Sleep(30000);
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "sleep")) {
        FILE *f = fopen(at("sec1.pid"), "w");
        if (f) { fprintf(f, "%lu\n", GetCurrentProcessId()); fclose(f); }
        printf("sleeping as %lu\n", GetCurrentProcessId());
        Sleep(60000);
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "wx")) {
        /* W^X: writable and executable at once is refused in VEILOS's
         * Security Mode (ERROR_DYNAMIC_CODE_BLOCKED, 1655); write, then
         * make executable, is not. */
        static const unsigned char ret42[] = { 0xB8, 42, 0, 0, 0, 0xC3 }; /* mov eax, 42; ret */
        DWORD old;
        void *p = VirtualAlloc(NULL, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE);
        show("allocate writable+executable", p != NULL);
        p = VirtualAlloc(NULL, 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        show("allocate writable", p != NULL);
        show("protect writable+executable", VirtualProtect(p, 4096, PAGE_EXECUTE_READWRITE, &old));
        memcpy(p, ret42, sizeof ret42);
        show("protect executable (after writing)", VirtualProtect(p, 4096, PAGE_EXECUTE_READ, &old));
        printf("generated code returned %d\n", ((int (*)(void))p)());
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "system")) {
        /* VEILOS only: \system is protected in Security Mode. */
        HANDLE h = CreateFileA("C:\\system\\sec1_test.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
        show("create a file in \\system", h != INVALID_HANDLE_VALUE);
        if (h != INVALID_HANDLE_VALUE) {
            CloseHandle(h);
            show("delete it", DeleteFileA("C:\\system\\sec1_test.txt"));
        }
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "poke")) {
        unsigned long pid = 0;
        HANDLE h;
        char buf[2];
        SIZE_T got = 0;
        FILE *f = fopen(at("sec1.pid"), "r");
        if (!f || fscanf(f, "%lu", &pid) != 1) { printf("no pid\n"); return 1; }
        fclose(f);
        h = OpenProcess(PROCESS_ALL_ACCESS, FALSE, pid);
        show("open an unrelated program", h != NULL);
        if (!h) return 0;
        show("read an unrelated program's memory", ReadProcessMemory(h, (LPCVOID)GetModuleHandleA(NULL), buf, 2, &got) && got == 2);
        show("end an unrelated program", TerminateProcess(h, 9));
        CloseHandle(h);
        return 0;
    }
    files();
    processes(argv[0]);
    sockets();
    printf("done\n");
    return 0;
}
