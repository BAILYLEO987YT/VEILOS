/* W18 (S3): what git asks of the system. Who owns a file, processes by id, a
 * program's own path, folder listings in every shape, byte order, processors. */
#include <winsock2.h>
#include <windows.h>
#include <aclapi.h>
#include <sddl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <io.h>
#include <fcntl.h>

static void sids(void)
{
    printf("--- SIDs\n");
    SID_IDENTIFIER_AUTHORITY nt = SECURITY_NT_AUTHORITY;
    PSID system = NULL, admins = NULL;
    printf("allocate: %d\n", AllocateAndInitializeSid(&nt, 1, SECURITY_LOCAL_SYSTEM_RID, 0, 0, 0, 0, 0, 0, 0, &system));
    printf("allocate two: %d\n", AllocateAndInitializeSid(&nt, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS, 0, 0, 0, 0, 0, 0, &admins));
    printf("valid: %d length %lu\n", IsValidSid(system), GetLengthSid(system));
    printf("length two: %lu\n", GetLengthSid(admins));
    char *text = NULL;
    ConvertSidToStringSidA(system, &text);
    printf("text: %s\n", text);
    LocalFree(text);
    ConvertSidToStringSidA(admins, &text);
    printf("text two: %s\n", text);
    LocalFree(text);
    wchar_t *wide = NULL;
    ConvertSidToStringSidW(system, &wide);
    printf("wide: %ls\n", wide);
    LocalFree(wide);
    printf("system is system: %d\n", IsWellKnownSid(system, WinLocalSystemSid));
    printf("system is admins: %d\n", IsWellKnownSid(system, WinBuiltinAdministratorsSid));
    printf("admins is admins: %d\n", IsWellKnownSid(admins, WinBuiltinAdministratorsSid));
    printf("equal to itself: %d\n", EqualSid(system, system));
    printf("equal to another: %d\n", EqualSid(system, admins));
    BYTE copy[64];
    BOOL copied = CopySid(sizeof copy, copy, admins);
    printf("copy: %d equal %d\n", copied, EqualSid(copy, admins));
    SetLastError(0);
    copied = CopySid(4, copy, admins);
    printf("copy too small: %d error %lu\n", copied, GetLastError());
    BOOL member = 99;
    printf("membership call: %d\n", CheckTokenMembership(NULL, admins, &member));
    FreeSid(system);
    FreeSid(admins);
}

/* The question behind git's "dubious ownership". */
static void owners(void)
{
    printf("--- owners\n");
    HANDLE token = NULL;
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token);
    BYTE info[256];
    DWORD n = 0;
    printf("token user: %d\n", GetTokenInformation(token, TokenUser, info, sizeof info, &n));
    PSID me = ((TOKEN_USER *)info)->User.Sid;
    CloseHandle(token);
    HANDLE f = CreateFileW(L"w18owned.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    CloseHandle(f);
    CreateDirectoryW(L"w18ownd", NULL);
    const wchar_t *paths[] = {L"w18owned.txt", L"w18ownd"};
    for (int i = 0; i < 2; i++) {
        PSID owner = NULL;
        PSECURITY_DESCRIPTOR sd = NULL;
        DWORD e = GetNamedSecurityInfoW(paths[i], SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &owner, NULL, NULL, NULL, &sd);
        printf("%ls: error %lu valid %d mine %d\n", paths[i], e, IsValidSid(owner),
               EqualSid(owner, me) || IsWellKnownSid(owner, WinBuiltinAdministratorsSid));
        LocalFree(sd);
    }
    PSID owner = NULL;
    PSECURITY_DESCRIPTOR sd = NULL;
    printf("missing: error %lu\n", GetNamedSecurityInfoW(L"w18none.txt", SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, &owner, NULL, NULL, NULL, &sd));
    DeleteFileW(L"w18owned.txt");
    RemoveDirectoryW(L"w18ownd");
}

static void byte_order(void)
{
    printf("--- byte order\n");
    printf("htons %04x ntohs %04x\n", htons(0x1234), ntohs(0x1234));
    printf("htonl %08lx ntohl %08lx\n", htonl(0x12345678), ntohl(0x12345678));
}

/* The child: a command line that doesn't name it, as when git runs "git". */
static int own_path_child(const char *argv0)
{
    char *pgm = NULL;
    char module[MAX_PATH];
    _get_pgmptr(&pgm);
    GetModuleFileNameA(NULL, module, MAX_PATH);
    int same = pgm && strcmp(pgm, module) == 0;
    int absolute = pgm && pgm[0] && pgm[1] == ':';
    int bare = strcmp(argv0, "bare") == 0;
    return 40 + same + 2 * absolute + 4 * bare;
}

static void processes(void)
{
    printf("--- processes by id\n");
    HANDLE self = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, GetCurrentProcessId());
    printf("self: %d\n", self != NULL);
    CloseHandle(self);
    SetLastError(0);
    HANDLE none = OpenProcess(PROCESS_QUERY_INFORMATION, FALSE, 0xFFFFFFF0);
    DWORD error = GetLastError();
    printf("no such id: %d error %lu\n", none != NULL, error);

    char exe[MAX_PATH], line[] = "bare child";
    GetModuleFileNameA(NULL, exe, MAX_PATH);
    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    printf("started: %d\n", CreateProcessA(exe, line, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi));
    /* The way git waits: by id. It must still answer once the child has ended. */
    HANDLE byid = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_INFORMATION, FALSE, pi.dwProcessId);
    printf("by id: %d\n", byid != NULL);
    printf("waited: %lu\n", WaitForSingleObject(byid, 20000));
    DWORD code = 0;
    GetExitCodeProcess(byid, &code);
    printf("its code: %lu\n", code);
    CloseHandle(byid);
    HANDLE again = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_INFORMATION, TRUE, pi.dwProcessId);
    printf("after its end: %d\n", again != NULL);
    DWORD flags = 0;
    GetHandleInformation(again, &flags);
    printf("inheritable when asked: %lu\n", flags & HANDLE_FLAG_INHERIT);
    code = 0;
    GetExitCodeProcess(again, &code);
    printf("its code again: %lu\n", code);
    CloseHandle(again);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
}

/* A copy has its own handle: git closes copies around every child it runs. */
static void descriptors(void)
{
    printf("--- descriptors\n");
    int fd = _open("w18dup.txt", _O_CREAT | _O_TRUNC | _O_RDWR | _O_BINARY, 0666);
    int copy = _dup(fd);
    printf("own handles: %d\n", _get_osfhandle(fd) != _get_osfhandle(copy));
    _write(copy, "one", 3);
    printf("position shared: %ld\n", _lseek(fd, 0, SEEK_CUR));
    _close(copy);
    printf("original after closing the copy: %d\n", _write(fd, "two", 3));
    copy = _dup(fd);
    CloseHandle((HANDLE)_get_osfhandle(copy));
    printf("original after closing the copy's handle: %d\n", _write(fd, "xy", 2));
    _dup2(fd, 20);
    printf("own handle at 20: %d\n", _get_osfhandle(fd) != _get_osfhandle(20));
    _close(fd);
    printf("20 after closing the first: %d\n", _write(20, "z", 1));
    printf("length: %ld\n", _lseek(20, 0, SEEK_END));
    _close(20);
    printf("removed: %d\n", _unlink("w18dup.txt"));
}

/* CONOUT$ is the console, not stdout: closing it must leave stdout alone. */
static void console_handle(void)
{
    printf("--- the console by name\n");
    fflush(stdout);
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    HANDLE con = CreateFileA("CONOUT$", GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
    int own = con != out;
    if (con != INVALID_HANDLE_VALUE)
        CloseHandle(con);
    DWORD n = 0;
    BOOL wrote = WriteFile(out, "still there\r\n", 13, &n, NULL);
    printf("own handle: %d, stdout after closing it: %d %lu\n", own, wrote, n);
}

typedef LONG(NTAPI *ListDir)(HANDLE, HANDLE, PVOID, PVOID, ULONG_PTR *, PVOID, ULONG, ULONG, BOOLEAN, PVOID, BOOLEAN);

static void list_folder(ListDir list, int cls, int name_at, BOOLEAN single)
{
    HANDLE dir = CreateFileA("w18dir", FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    static BYTE buf[8192];
    ULONG_PTR iosb[2];
    printf("class %d, single %d:", cls, single);
    for (;;) {
        LONG s = list(dir, NULL, NULL, NULL, iosb, buf, sizeof buf, cls, single, NULL, FALSE);
        if (s == (LONG)0x80000006) { /* STATUS_NO_MORE_FILES */
            printf(" end");
            break;
        }
        if (s < 0) {
            printf(" status %08lx", (unsigned long)s);
            break;
        }
        BYTE *e = buf;
        for (;;) {
            ULONG next = *(ULONG *)e;
            LONGLONG size = *(LONGLONG *)(e + 40);
            ULONG attrs = *(ULONG *)(e + 56);
            ULONG bytes = *(ULONG *)(e + 60);
            printf(" %.*ls/%d/%lld", (int)(bytes / 2), (wchar_t *)(e + name_at), (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0,
                   (attrs & FILE_ATTRIBUTE_DIRECTORY) ? 0 : size);
            if (!next)
                break;
            e += next;
        }
    }
    printf("\n");
    CloseHandle(dir);
}

static void listings(void)
{
    printf("--- folder listings\n");
    CreateDirectoryA("w18dir", NULL);
    HANDLE f = CreateFileA("w18dir\\a.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    DWORD n;
    WriteFile(f, "hello", 5, &n, NULL);
    CloseHandle(f);
    CreateDirectoryA("w18dir\\sub", NULL);
    ListDir list = (ListDir)GetProcAddress(GetModuleHandleA("ntdll.dll"), "NtQueryDirectoryFile");
    int classes[][2] = {{1, 64}, {2, 68}, {3, 94}, {37, 104}, {38, 80}};
    for (int i = 0; i < 5; i++) {
        list_folder(list, classes[i][0], classes[i][1], TRUE);
        list_folder(list, classes[i][0], classes[i][1], FALSE);
    }
    HANDLE dir = CreateFileA("w18dir", FILE_LIST_DIRECTORY, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
    static BYTE buf[256];
    ULONG_PTR iosb[2];
    printf("unknown class: %08lx\n", (unsigned long)list(dir, NULL, NULL, NULL, iosb, buf, sizeof buf, 9999, TRUE, NULL, FALSE));
    CloseHandle(dir);
    DeleteFileA("w18dir\\a.txt");
    RemoveDirectoryA("w18dir\\sub");
    RemoveDirectoryA("w18dir");
}

static int bits(ULONG_PTR v)
{
    int n = 0;
    for (; v; v &= v - 1)
        n++;
    return n;
}

static void processors(void)
{
    printf("--- processors\n");
    SYSTEM_INFO si;
    GetSystemInfo(&si);
    static BYTE buf[65536];
    DWORD size = 0;
    SetLastError(0);
    BOOL ok = GetLogicalProcessorInformationEx(RelationProcessorCore, NULL, &size);
    DWORD error = GetLastError();
    printf("asking the size: %d error %lu, has size %d\n", ok, error, size > 0);
    size = sizeof buf;
    printf("cores: %d\n", GetLogicalProcessorInformationEx(RelationProcessorCore, (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)buf, &size));
    int total = 0;
    for (BYTE *p = buf; p < buf + size; p += ((PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)p)->Size) {
        PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX e = (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)p;
        if (e->Relationship != RelationProcessorCore)
            printf("wrong relation %d\n", e->Relationship);
        total += bits(e->Processor.GroupMask[0].Mask);
    }
    printf("every CPU in a core: %d\n", total == (int)si.dwNumberOfProcessors);
    size = sizeof buf;
    printf("groups: %d\n", GetLogicalProcessorInformationEx(RelationGroup, (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)buf, &size));
    PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX g = (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)buf;
    printf("one group: %d, all active: %d, mask: %d\n", g->Group.ActiveGroupCount,
           g->Group.GroupInfo[0].ActiveProcessorCount == si.dwNumberOfProcessors,
           bits(g->Group.GroupInfo[0].ActiveProcessorMask) == (int)si.dwNumberOfProcessors);
    size = sizeof buf;
    printf("packages: %d\n", GetLogicalProcessorInformationEx(RelationProcessorPackage, (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)buf, &size));
    printf("all CPUs in the package: %d\n", bits(g->Processor.GroupMask[0].Mask) == (int)si.dwNumberOfProcessors);
    size = sizeof buf;
    printf("NUMA: %d\n", GetLogicalProcessorInformationEx(RelationNumaNode, (PSYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)buf, &size));
    printf("all CPUs in the node: %d\n", bits(g->NumaNode.GroupMask.Mask) == (int)si.dwNumberOfProcessors);
    USHORT count = 0, groups[4] = {99, 99, 99, 99};
    SetLastError(0);
    ok = GetProcessGroupAffinity(GetCurrentProcess(), &count, groups);
    error = GetLastError();
    printf("process groups, no room: %d error %lu count %u\n", ok, error, count);
    count = 4;
    ok = GetProcessGroupAffinity(GetCurrentProcess(), &count, groups);
    printf("process groups: %d count %u first %u\n", ok, count, groups[0]);
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "child") == 0)
        return own_path_child(argv[0]);
    setvbuf(stdout, NULL, _IONBF, 0);
    sids();
    owners();
    byte_order();
    processes();
    descriptors();
    console_handle();
    listings();
    processors();
    printf("done\n");
    return 5;
}
