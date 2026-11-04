/* W16 (S1): file semantics the Rust toolchain and git lean on: partial reads
 * and writes, two handles seeing each other, sparse growth and truncation,
 * rename (open files, replacing, directories, between folders), deletion of an
 * open file, and file times. Works in a folder of its own under the current
 * directory, and removes it. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define DIR "w16tmp"

static void err(const char *what, BOOL ok)
{
    printf("%s: %d err %lu\n", what, ok ? 1 : 0, ok ? 0ul : GetLastError());
}

static HANDLE open_rw(const char *path, DWORD disposition)
{
    return CreateFileA(path, GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        NULL, disposition, FILE_ATTRIBUTE_NORMAL, NULL);
}

static void put(const char *path, const char *text)
{
    HANDLE h = open_rw(path, CREATE_ALWAYS);
    DWORD n;
    WriteFile(h, text, (DWORD)strlen(text), &n, NULL);
    CloseHandle(h);
}

static void show(const char *path)
{
    char buf[256];
    DWORD n = 0;
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, NULL, OPEN_EXISTING, 0, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        printf("%s: missing, err %lu\n", path, GetLastError());
        return;
    }
    ReadFile(h, buf, sizeof buf - 1, &n, NULL);
    buf[n] = 0;
    printf("%s: %lu bytes \"%s\"\n", path, n, buf);
    CloseHandle(h);
}

static long long size_of(HANDLE h)
{
    LARGE_INTEGER s;
    s.QuadPart = -1;
    GetFileSizeEx(h, &s);
    return s.QuadPart;
}

static void partial(void)
{
    printf("--- partial reads and writes\n");
    HANDLE h = open_rw(DIR "\\big.bin", CREATE_ALWAYS);
    /* Larger than any buffer or cluster, each block with its number in it. */
    static unsigned block[1024];
    DWORD n;
    for (int b = 0; b < 300; b++) {
        for (int i = 0; i < 1024; i++) block[i] = b * 1024 + i;
        WriteFile(h, block, sizeof block, &n, NULL);
    }
    printf("size %lld\n", size_of(h));
    /* Reads across block boundaries, at odd offsets. */
    static const long long offsets[] = { 0, 4, 4092, 4097, 65536, 1048572, 1228796 };
    for (int k = 0; k < 7; k++) {
        unsigned char b[16];
        OVERLAPPED ov;
        memset(&ov, 0, sizeof ov);
        memset(b, 0, sizeof b);
        ov.Offset = (DWORD)offsets[k];
        BOOL ok = ReadFile(h, b, 12, &n, &ov);
        printf("at %lld: %d n=%lu %02x%02x%02x%02x %02x%02x%02x%02x\n", offsets[k], ok, n, b[0], b[1], b[2], b[3], b[8], b[9], b[10], b[11]);
    }
    /* Overwrite across a boundary, leaving the size alone. */
    LARGE_INTEGER at;
    at.QuadPart = 4094;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    WriteFile(h, "ABCDEFGH", 8, &n, NULL);
    at.QuadPart = 4090;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    unsigned char c[20];
    ReadFile(h, c, 16, &n, NULL);
    printf("around the boundary:");
    for (int i = 0; i < 16; i++) printf(" %02x", c[i]);
    printf("\nsize %lld\n", size_of(h));
    /* A read past the end. */
    at.QuadPart = 2000000;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    n = 77;
    BOOL ok = ReadFile(h, c, 16, &n, NULL);
    printf("past the end: %d n=%lu\n", ok, n);
    CloseHandle(h);
}

static void growth(void)
{
    printf("--- growing and truncating\n");
    HANDLE h = open_rw(DIR "\\sparse.bin", CREATE_ALWAYS);
    LARGE_INTEGER at;
    DWORD n;
    at.QuadPart = 200000;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    WriteFile(h, "END", 3, &n, NULL);
    printf("size %lld\n", size_of(h));
    unsigned char b[8];
    at.QuadPart = 0;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    ReadFile(h, b, 8, &n, NULL);
    printf("start reads zero: %d\n", memcmp(b, "\0\0\0\0\0\0\0\0", 8) == 0);
    at.QuadPart = 150000;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    ReadFile(h, b, 8, &n, NULL);
    printf("middle reads zero: %d\n", memcmp(b, "\0\0\0\0\0\0\0\0", 8) == 0);
    at.QuadPart = 10;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    err("set end", SetEndOfFile(h));
    printf("size %lld\n", size_of(h));
    at.QuadPart = 100;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    err("set end", SetEndOfFile(h));
    printf("size %lld\n", size_of(h));
    at.QuadPart = 50;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    memset(b, 1, 8);
    ReadFile(h, b, 8, &n, NULL);
    printf("regrown reads zero: %d\n", memcmp(b, "\0\0\0\0\0\0\0\0", 8) == 0);
    at.QuadPart = 0;
    SetFilePointerEx(h, at, NULL, FILE_BEGIN);
    err("empty", SetEndOfFile(h));
    printf("size %lld\n", size_of(h));
    WriteFile(h, "again", 5, &n, NULL);
    CloseHandle(h);
    show(DIR "\\sparse.bin");
}

static void two_handles(void)
{
    printf("--- two handles to one file\n");
    HANDLE a = open_rw(DIR "\\two.txt", CREATE_ALWAYS);
    HANDLE b = open_rw(DIR "\\two.txt", OPEN_EXISTING);
    DWORD n;
    char buf[32];
    WriteFile(a, "from a", 6, &n, NULL);
    printf("b sees size %lld\n", size_of(b));
    memset(buf, 0, sizeof buf);
    ReadFile(b, buf, 31, &n, NULL);
    printf("b reads \"%s\"\n", buf);
    WriteFile(b, " and b", 6, &n, NULL);
    printf("a sees size %lld\n", size_of(a));
    /* Each handle has its own position. */
    LARGE_INTEGER zero, pa, pb;
    zero.QuadPart = 0;
    SetFilePointerEx(a, zero, &pa, FILE_CURRENT);
    SetFilePointerEx(b, zero, &pb, FILE_CURRENT);
    printf("positions %lld %lld\n", pa.QuadPart, pb.QuadPart);
    CloseHandle(a);
    show(DIR "\\two.txt");
    CloseHandle(b);
    /* A new open sees it all at once; truncating reaches other handles. */
    a = open_rw(DIR "\\two.txt", OPEN_EXISTING);
    b = open_rw(DIR "\\two.txt", TRUNCATE_EXISTING);
    printf("after truncate a sees %lld\n", size_of(a));
    CloseHandle(a);
    CloseHandle(b);
}

static void renames(void)
{
    printf("--- rename\n");
    put(DIR "\\a.txt", "file a");
    err("plain", MoveFileA(DIR "\\a.txt", DIR "\\b.txt"));
    show(DIR "\\a.txt");
    show(DIR "\\b.txt");

    put(DIR "\\c.txt", "file c");
    err("onto existing, no flag", MoveFileExA(DIR "\\c.txt", DIR "\\b.txt", 0));
    show(DIR "\\b.txt");
    err("onto existing, replace", MoveFileExA(DIR "\\c.txt", DIR "\\b.txt", MOVEFILE_REPLACE_EXISTING));
    show(DIR "\\b.txt");
    show(DIR "\\c.txt");

    err("missing source", MoveFileA(DIR "\\nope.txt", DIR "\\x.txt"));
    err("missing folder", MoveFileA(DIR "\\b.txt", DIR "\\nodir\\x.txt"));

    err("case only", MoveFileA(DIR "\\b.txt", DIR "\\B.TXT"));
    WIN32_FIND_DATAA fd;
    HANDLE f = FindFirstFileA(DIR "\\b.txt", &fd);
    printf("found as %s\n", f == INVALID_HANDLE_VALUE ? "(none)" : fd.cFileName);
    if (f != INVALID_HANDLE_VALUE) FindClose(f);

    err("mkdir", CreateDirectoryA(DIR "\\sub", NULL));
    err("into a folder", MoveFileA(DIR "\\B.TXT", DIR "\\sub\\moved with spaces and a long name.txt"));
    show(DIR "\\sub\\moved with spaces and a long name.txt");

    /* A folder, with what's in it. */
    err("folder", MoveFileA(DIR "\\sub", DIR "\\renamed"));
    show(DIR "\\renamed\\moved with spaces and a long name.txt");
    err("mkdir", CreateDirectoryA(DIR "\\outer", NULL));
    err("folder into folder", MoveFileA(DIR "\\renamed", DIR "\\outer\\inner"));
    show(DIR "\\outer\\inner\\moved with spaces and a long name.txt");
    /* ".." of a moved folder is right. */
    show(DIR "\\outer\\inner\\..\\inner\\moved with spaces and a long name.txt");
    put(DIR "\\outer\\inner\\new.txt", "made after the move");
    show(DIR "\\outer\\inner\\new.txt");
    err("folder into itself", MoveFileA(DIR "\\outer", DIR "\\outer\\inner\\x"));

    /* An open file (shared for delete), and writing to it afterwards. */
    put(DIR "\\open.txt", "before");
    HANDLE h = open_rw(DIR "\\open.txt", OPEN_EXISTING);
    err("open file", MoveFileA(DIR "\\open.txt", DIR "\\moved.txt"));
    DWORD n;
    LARGE_INTEGER end;
    end.QuadPart = 0;
    SetFilePointerEx(h, end, NULL, FILE_END);
    WriteFile(h, " after", 6, &n, NULL);
    CloseHandle(h);
    show(DIR "\\open.txt");
    show(DIR "\\moved.txt");

    /* Replacing a file that's open is refused. */
    put(DIR "\\p.txt", "p");
    put(DIR "\\q.txt", "q");
    h = open_rw(DIR "\\q.txt", OPEN_EXISTING);
    err("onto an open file", MoveFileExA(DIR "\\p.txt", DIR "\\q.txt", MOVEFILE_REPLACE_EXISTING));
    CloseHandle(h);
    show(DIR "\\q.txt");
}

static void deletes(void)
{
    printf("--- deleting\n");
    put(DIR "\\d.txt", "doomed");
    HANDLE h = open_rw(DIR "\\d.txt", OPEN_EXISTING);
    err("delete while open", DeleteFileA(DIR "\\d.txt"));
    /* Still readable through the handle, and not openable again. */
    char buf[16];
    DWORD n;
    memset(buf, 0, sizeof buf);
    ReadFile(h, buf, 15, &n, NULL);
    printf("through the handle: \"%s\"\n", buf);
    HANDLE again = open_rw(DIR "\\d.txt", OPEN_EXISTING);
    printf("open again: %d err %lu\n", again != INVALID_HANDLE_VALUE, GetLastError());
    if (again != INVALID_HANDLE_VALUE) CloseHandle(again);
    CloseHandle(h);
    printf("after close: %lu err %lu\n", GetFileAttributesA(DIR "\\d.txt") == INVALID_FILE_ATTRIBUTES, GetLastError());

    h = CreateFileA(DIR "\\t.txt", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE, NULL);
    printf("delete on close, open: %d\n", GetFileAttributesA(DIR "\\t.txt") != INVALID_FILE_ATTRIBUTES);
    CloseHandle(h);
    printf("delete on close, closed: %d\n", GetFileAttributesA(DIR "\\t.txt") != INVALID_FILE_ATTRIBUTES);

    put(DIR "\\e.txt", "disposition");
    h = CreateFileA(DIR "\\e.txt", DELETE, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    FILE_DISPOSITION_INFO di = { TRUE };
    err("disposition", SetFileInformationByHandle(h, FileDispositionInfo, &di, sizeof di));
    CloseHandle(h);
    printf("after: %d\n", GetFileAttributesA(DIR "\\e.txt") != INVALID_FILE_ATTRIBUTES);

    err("mkdir", CreateDirectoryA(DIR "\\full", NULL));
    put(DIR "\\full\\x", "x");
    err("full folder", RemoveDirectoryA(DIR "\\full"));
    DeleteFileA(DIR "\\full\\x");
    err("emptied folder", RemoveDirectoryA(DIR "\\full"));
}

/* Rename by handle, the way std::fs::rename may. */
static void by_handle(void)
{
    printf("--- rename by handle\n");
    put(DIR "\\h1.txt", "by handle");
    HANDLE h = CreateFileA(DIR "\\h1.txt", DELETE | SYNCHRONIZE, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    static const WCHAR target[] = L"h2.txt";
    char store[sizeof(FILE_RENAME_INFO) + 1024];
    FILE_RENAME_INFO *ri = (FILE_RENAME_INFO *)store;
    memset(store, 0, sizeof store);
    /* A whole path is what the call wants. */
    WCHAR full[512];
    GetFullPathNameW(L"w16tmp\\h2.txt", 512, full, NULL);
    ri->FileNameLength = (DWORD)(wcslen(full) * 2);
    memcpy(ri->FileName, full, ri->FileNameLength);
    (void)target;
    err("rename info", SetFileInformationByHandle(h, FileRenameInfo, ri, (DWORD)(sizeof(FILE_RENAME_INFO) + ri->FileNameLength)));
    CloseHandle(h);
    show(DIR "\\h1.txt");
    show(DIR "\\h2.txt");
}

static void times(void)
{
    printf("--- times\n");
    put(DIR "\\time.txt", "t");
    HANDLE h = open_rw(DIR "\\time.txt", OPEN_EXISTING);
    FILETIME w, got;
    /* 2020-06-15 12:00:00 UTC, on an even second for FAT. */
    SYSTEMTIME st = { 2020, 6, 1, 15, 12, 0, 0, 0 };
    SystemTimeToFileTime(&st, &w);
    err("set", SetFileTime(h, NULL, NULL, &w));
    err("get", GetFileTime(h, NULL, NULL, &got));
    FileTimeToSystemTime(&got, &st);
    printf("by handle %u-%02u-%02u %02u:%02u\n", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute);
    CloseHandle(h);
    WIN32_FILE_ATTRIBUTE_DATA a;
    err("attributes", GetFileAttributesExA(DIR "\\time.txt", GetFileExInfoStandard, &a));
    FileTimeToSystemTime(&a.ftLastWriteTime, &st);
    printf("by path %u-%02u-%02u %02u:%02u size %lu\n", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, a.nFileSizeLow);
    WIN32_FIND_DATAA fd;
    HANDLE f = FindFirstFileA(DIR "\\time.txt", &fd);
    FileTimeToSystemTime(&fd.ftLastWriteTime, &st);
    printf("in a listing %u-%02u-%02u %02u:%02u\n", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute);
    FindClose(f);
    /* A rename keeps the time. */
    MoveFileA(DIR "\\time.txt", DIR "\\time2.txt");
    GetFileAttributesExA(DIR "\\time2.txt", GetFileExInfoStandard, &a);
    FileTimeToSystemTime(&a.ftLastWriteTime, &st);
    printf("after rename %u-%02u-%02u\n", st.wYear, st.wMonth, st.wDay);
    /* A write moves it on from 2020. */
    h = open_rw(DIR "\\time2.txt", OPEN_EXISTING);
    DWORD n;
    WriteFile(h, "more", 4, &n, NULL);
    CloseHandle(h);
    GetFileAttributesExA(DIR "\\time2.txt", GetFileExInfoStandard, &a);
    FileTimeToSystemTime(&a.ftLastWriteTime, &st);
    printf("after a write, later than 2020: %d\n", st.wYear > 2020);
}

/* Remove what the tests left. */
static void cleanup(const char *dir)
{
    char pattern[MAX_PATH], path[MAX_PATH];
    WIN32_FIND_DATAA fd;
    snprintf(pattern, sizeof pattern, "%s\\*", dir);
    HANDLE f = FindFirstFileA(pattern, &fd);
    if (f != INVALID_HANDLE_VALUE) {
        do {
            if (!strcmp(fd.cFileName, ".") || !strcmp(fd.cFileName, "..")) continue;
            snprintf(path, sizeof path, "%s\\%s", dir, fd.cFileName);
            if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) cleanup(path);
            else DeleteFileA(path);
        } while (FindNextFileA(f, &fd));
        FindClose(f);
    }
    RemoveDirectoryA(dir);
}

int main(void)
{
    cleanup(DIR);
    err("mkdir", CreateDirectoryA(DIR, NULL));
    partial();
    growth();
    two_handles();
    renames();
    deletes();
    by_handle();
    times();
    cleanup(DIR);
    printf("gone: %d\n", GetFileAttributesA(DIR) == INVALID_FILE_ATTRIBUTES);
    printf("done\n");
    return 5;
}
