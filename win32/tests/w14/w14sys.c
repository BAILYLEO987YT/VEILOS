/* W14: the registry, INI files and version resources. On Windows this touches
 * only HKCU\Software\VeilTestW14, which it deletes, and a file in the temp
 * folder. */
#include <windows.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

#pragma comment(lib, "advapi32.lib")
#pragma comment(lib, "version.lib")

static void wprint(const char *label, const WCHAR *s)
{
    char buf[512];
    WideCharToMultiByte(CP_UTF8, 0, s, -1, buf, sizeof buf, NULL, NULL);
    printf("%s: \"%s\"\n", label, buf);
}

#define TESTKEY L"Software\\VeilTestW14"

static void registry(void)
{
    printf("--- registry\n");
    HKEY k = NULL, sub = NULL;
    DWORD disp = 0;
    RegDeleteTreeW(HKEY_CURRENT_USER, TESTKEY);
    LONG e = RegOpenKeyExW(HKEY_CURRENT_USER, TESTKEY, 0, KEY_READ, &k);
    printf("open missing: %ld, handle null %d\n", e, k == NULL);
    e = RegCreateKeyExW(HKEY_CURRENT_USER, TESTKEY L"\\A\\B", 0, NULL, 0, KEY_ALL_ACCESS, NULL, &sub, &disp);
    printf("create deep: %ld disp %lu\n", e, disp);
    RegCloseKey(sub);
    e = RegCreateKeyExW(HKEY_CURRENT_USER, TESTKEY, 0, NULL, 0, KEY_ALL_ACCESS, NULL, &k, &disp);
    printf("create existing: %ld disp %lu\n", e, disp);

    const WCHAR *text = L"hello";
    printf("set sz %ld\n", RegSetValueExW(k, L"Text", 0, REG_SZ, (const BYTE *)text, 12));
    DWORD d = 0x12345678;
    printf("set dword %ld\n", RegSetValueExW(k, L"Number", 0, REG_DWORD, (const BYTE *)&d, 4));
    ULONGLONG q = 0x1122334455667788ULL;
    printf("set qword %ld\n", RegSetValueExW(k, L"Big", 0, REG_QWORD, (const BYTE *)&q, 8));
    BYTE bin[3] = {1, 2, 3};
    printf("set binary %ld\n", RegSetValueExW(k, L"Bin", 0, REG_BINARY, bin, 3));
    const WCHAR multi[] = L"one\0two\0";
    printf("set multi %ld\n", RegSetValueExW(k, L"Multi", 0, REG_MULTI_SZ, (const BYTE *)multi, sizeof multi));
    printf("set default %ld\n", RegSetValueExW(k, NULL, 0, REG_SZ, (const BYTE *)L"dflt", 10));
    printf("set no terminator %ld\n", RegSetValueExW(k, L"Bare", 0, REG_SZ, (const BYTE *)L"abc", 6));
    printf("set expand %ld\n", RegSetValueExW(k, L"Exp", 0, REG_EXPAND_SZ, (const BYTE *)L"a%W14VAR%b", 22));

    WCHAR buf[64];
    DWORD kind = 0, size = sizeof buf;
    e = RegQueryValueExW(k, L"text", NULL, &kind, (BYTE *)buf, &size);
    printf("query (any case) %ld kind %lu size %lu", e, kind, size);
    wprint("", buf);
    size = 0;
    e = RegQueryValueExW(k, L"Text", NULL, &kind, NULL, &size);
    printf("size only %ld size %lu\n", e, size);
    size = 4;
    e = RegQueryValueExW(k, L"Text", NULL, &kind, (BYTE *)buf, &size);
    printf("too small %ld size %lu\n", e, size);
    size = sizeof buf;
    e = RegQueryValueExW(k, L"Nope", NULL, &kind, (BYTE *)buf, &size);
    printf("missing %ld\n", e);
    size = sizeof d;
    d = 0;
    e = RegQueryValueExW(k, L"Number", NULL, &kind, (BYTE *)&d, &size);
    printf("dword %ld kind %lu %#lx\n", e, kind, d);
    size = sizeof buf;
    e = RegQueryValueExW(k, NULL, NULL, &kind, (BYTE *)buf, &size);
    printf("default %ld", e);
    wprint("", buf);

    /* RegGetValue's rules. */
    size = sizeof buf;
    e = RegGetValueW(k, NULL, L"Bare", RRF_RT_REG_SZ, &kind, buf, &size);
    printf("get terminates %ld size %lu", e, size);
    wprint("", buf);
    size = sizeof d;
    e = RegGetValueW(k, NULL, L"Text", RRF_RT_REG_DWORD, &kind, &d, &size);
    printf("wrong type %ld\n", e);
    size = sizeof d;
    e = RegGetValueW(HKEY_CURRENT_USER, TESTKEY, L"Number", RRF_RT_DWORD, &kind, &d, &size);
    printf("through a subkey path %ld %#lx\n", e, d);
    SetEnvironmentVariableW(L"W14VAR", L"XYZ");
    size = sizeof buf;
    e = RegGetValueW(k, NULL, L"Exp", RRF_RT_REG_SZ, &kind, buf, &size);
    printf("expanded %ld kind %lu size %lu", e, kind, size);
    wprint("", buf);
    size = sizeof buf;
    e = RegGetValueW(k, NULL, L"Exp", RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, &kind, buf, &size);
    printf("not expanded %ld kind %lu", e, kind);
    wprint("", buf);

    /* The ANSI side. */
    char a[64];
    size = sizeof a;
    e = RegQueryValueExA(k, "Text", NULL, &kind, (BYTE *)a, &size);
    printf("ansi query %ld kind %lu size %lu \"%s\"\n", e, kind, size, a);
    printf("ansi set %ld\n", RegSetValueExA(k, "FromA", 0, REG_SZ, (const BYTE *)"narrow", 7));
    size = sizeof buf;
    e = RegQueryValueExW(k, L"FromA", NULL, &kind, (BYTE *)buf, &size);
    printf("read wide %ld size %lu", e, size);
    wprint("", buf);
    size = 0;
    e = RegQueryValueExA(k, "Number", NULL, &kind, NULL, &size);
    printf("ansi size of dword %ld %lu\n", e, size);

    /* Listing. */
    DWORD nsub = 0, maxsub = 0, nval = 0, maxname = 0, maxdata = 0;
    e = RegQueryInfoKeyW(k, NULL, NULL, NULL, &nsub, &maxsub, NULL, &nval, &maxname, &maxdata, NULL, NULL);
    printf("info %ld subkeys %lu longest %lu values %lu longest name %lu largest data %lu\n", e, nsub, maxsub, nval, maxname, maxdata);
    for (DWORD i = 0;; i++) {
        WCHAR name[64];
        DWORD nl = 64, dl = sizeof buf;
        e = RegEnumValueW(k, i, name, &nl, NULL, &kind, (BYTE *)buf, &dl);
        if (e != 0) {
            printf("enum values end %ld at %lu\n", e, i);
            break;
        }
        printf("  value %lu len %lu kind %lu size %lu", i, nl, kind, dl);
        wprint(" name", name);
    }
    for (DWORD i = 0;; i++) {
        WCHAR name[64];
        DWORD nl = 64;
        e = RegEnumKeyExW(k, i, name, &nl, NULL, NULL, NULL, NULL);
        if (e != 0) {
            printf("enum keys end %ld at %lu\n", e, i);
            break;
        }
        printf("  key %lu len %lu", i, nl);
        wprint(" name", name);
    }
    WCHAR tiny[2];
    DWORD tl = 1;
    e = RegEnumKeyExW(k, 0, tiny, &tl, NULL, NULL, NULL, NULL);
    printf("enum into a small buffer %ld\n", e);

    /* Sorted subkeys. */
    RegCreateKeyW(k, L"zeta", &sub);
    RegCloseKey(sub);
    RegCreateKeyW(k, L"Beta", &sub);
    RegCloseKey(sub);
    RegCreateKeyW(k, L"alpha", &sub);
    RegCloseKey(sub);
    for (DWORD i = 0;; i++) {
        WCHAR name[64];
        DWORD nl = 64;
        if (RegEnumKeyExW(k, i, name, &nl, NULL, NULL, NULL, NULL) != 0)
            break;
        wprint("  sorted", name);
    }

    /* Deleting. */
    printf("delete value %ld, again %ld\n", RegDeleteValueW(k, L"FromA"), RegDeleteValueW(k, L"FromA"));
    printf("delete key with subkeys %ld\n", RegDeleteKeyW(k, L"A"));
    printf("delete leaf %ld\n", RegDeleteKeyW(k, L"A\\B"));
    printf("delete now-empty %ld\n", RegDeleteKeyW(k, L"A"));
    printf("delete missing %ld\n", RegDeleteKeyW(k, L"A"));
    printf("set key value %ld\n", RegSetKeyValueW(k, L"made\\by\\set", L"v", REG_SZ, L"x", 4));
    RegCloseKey(k);
    printf("use after close %ld\n", RegSetValueExW(k, L"x", 0, REG_SZ, (const BYTE *)L"y", 4));
    printf("delete tree %ld\n", RegDeleteTreeW(HKEY_CURRENT_USER, TESTKEY));
    printf("gone %ld\n", RegOpenKeyExW(HKEY_CURRENT_USER, TESTKEY, 0, KEY_READ, &k));
    printf("open a root with no subkey %ld\n", RegOpenKeyExW(HKEY_LOCAL_MACHINE, NULL, 0, KEY_READ, &k));
    if (k)
        RegCloseKey(k);
}

/* -----| INI files |----- */

static void show_list(const char *label, const char *p)
{
    printf("%s:", label);
    while (*p) {
        printf(" [%s]", p);
        p += strlen(p) + 1;
    }
    printf("\n");
}

static void dump(const char *path)
{
    HANDLE h = CreateFileA(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    char b[1024] = {0};
    DWORD n = 0;
    if (h != INVALID_HANDLE_VALUE) {
        ReadFile(h, b, sizeof b - 1, &n, NULL);
        CloseHandle(h);
    }
    printf("file (%lu):\n", n);
    for (char *p = b; *p; p++) {
        if (*p == '\r')
            printf("\\r");
        else if (*p == '\n')
            printf("\\n\n");
        else
            putchar(*p);
    }
    printf("<end>\n");
}

static void ini(void)
{
    printf("--- ini\n");
    char path[MAX_PATH];
    GetTempPathA(MAX_PATH, path);
    strcat(path, "w14test.ini");
    DeleteFileA(path);
    char b[256];
    DWORD n = GetPrivateProfileStringA("S", "k", "default ", b, sizeof b, path);
    printf("no file: %lu \"%s\"\n", n, b);
    printf("int default %u\n", GetPrivateProfileIntA("S", "k", 42, path));

    HANDLE h = CreateFileA(path, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
    const char *text = "; comment\r\n[First]\r\nname = value here  \r\nQuoted=\"in quotes\"\r\nnum=123\r\nneg=-7\r\nhex=0x10\r\nbad=12abc\r\nempty=\r\n; note=no\r\nnoequals\r\n\r\n[Second]\r\nx=1\r\ny=2\r\n";
    DWORD done;
    WriteFile(h, text, (DWORD)strlen(text), &done, NULL);
    CloseHandle(h);

    n = GetPrivateProfileStringA("first", "NAME", "d", b, sizeof b, path);
    printf("any case, trimmed: %lu \"%s\"\n", n, b);
    n = GetPrivateProfileStringA("First", "Quoted", "d", b, sizeof b, path);
    printf("quotes: %lu \"%s\"\n", n, b);
    n = GetPrivateProfileStringA("First", "empty", "d", b, sizeof b, path);
    printf("empty: %lu \"%s\"\n", n, b);
    n = GetPrivateProfileStringA("First", "note", "d", b, sizeof b, path);
    printf("comment: %lu \"%s\"\n", n, b);
    n = GetPrivateProfileStringA("First", "name", "d", b, 4, path);
    printf("cut: %lu \"%s\"\n", n, b);
    n = GetPrivateProfileStringA(NULL, NULL, "d", b, sizeof b, path);
    printf("sections %lu ", n);
    show_list("", b);
    n = GetPrivateProfileStringA("Second", NULL, "d", b, sizeof b, path);
    printf("keys %lu ", n);
    show_list("", b);
    n = GetPrivateProfileSectionA("Second", b, sizeof b, path);
    printf("section %lu ", n);
    show_list("", b);
    n = GetPrivateProfileSectionNamesA(b, sizeof b, path);
    printf("names %lu ", n);
    show_list("", b);
    n = GetPrivateProfileStringA(NULL, NULL, "d", b, 5, path);
    printf("sections cut %lu\n", n);
    printf("ints %u %u %u %u %u\n", GetPrivateProfileIntA("First", "num", 0, path), GetPrivateProfileIntA("First", "neg", 0, path), GetPrivateProfileIntA("First", "hex", 0, path),
           GetPrivateProfileIntA("First", "bad", 0, path), GetPrivateProfileIntA("First", "empty", 5, path));
    WCHAR w[128], wpath[MAX_PATH];
    MultiByteToWideChar(CP_ACP, 0, path, -1, wpath, MAX_PATH);
    n = GetPrivateProfileStringW(L"Second", L"y", L"d", w, 128, wpath);
    printf("wide %lu", n);
    wprint("", w);
    n = GetPrivateProfileStringW(L"Second", NULL, L"d", w, 128, wpath);
    printf("wide keys %lu\n", n);
    printf("wide int %u\n", GetPrivateProfileIntW(L"First", L"num", 0, wpath));

    printf("change %d\n", WritePrivateProfileStringA("Second", "x", "changed", path));
    printf("add %d\n", WritePrivateProfileStringA("First", "added", "new", path));
    printf("new section %d\n", WritePrivateProfileStringA("Third", "z", "9", path));
    dump(path);
    printf("delete key %d\n", WritePrivateProfileStringA("First", "num", NULL, path));
    printf("delete section %d\n", WritePrivateProfileStringA("Second", NULL, NULL, path));
    dump(path);
    printf("wide write %d\n", WritePrivateProfileStringW(L"Third", L"w", L"wide", wpath));
    n = GetPrivateProfileStringA("Third", "w", "d", b, sizeof b, path);
    printf("back %lu \"%s\"\n", n, b);
    DeleteFileA(path);
    printf("fresh file %d\n", WritePrivateProfileStringA("New", "k", "v", path));
    dump(path);
    DeleteFileA(path);
}

/* -----| version resources |----- */

static void version(void)
{
    printf("--- version\n");
    WCHAR self[MAX_PATH];
    GetModuleFileNameW(NULL, self, MAX_PATH);
    DWORD handle = 7;
    DWORD size = GetFileVersionInfoSizeW(self, &handle);
    printf("has one %d, handle %lu\n", size > 0, handle);
    BYTE *block = (BYTE *)HeapAlloc(GetProcessHeap(), 0, size);
    printf("got %d\n", GetFileVersionInfoW(self, 0, size, block));
    VS_FIXEDFILEINFO *fixed = NULL;
    UINT len = 0;
    printf("root %d len %u\n", VerQueryValueW(block, L"\\", (void **)&fixed, &len), len);
    printf("signature %#lx file %#lx.%#lx product %#lx.%#lx\n", fixed->dwSignature, fixed->dwFileVersionMS, fixed->dwFileVersionLS, fixed->dwProductVersionMS, fixed->dwProductVersionLS);
    DWORD *tr = NULL;
    printf("translation %d len %u\n", VerQueryValueW(block, L"\\VarFileInfo\\Translation", (void **)&tr, &len), len);
    printf("  %#lx\n", *tr);
    WCHAR *s = NULL;
    printf("string %d len %u\n", VerQueryValueW(block, L"\\StringFileInfo\\040904B0\\FileDescription", (void **)&s, &len), len);
    wprint("  value", s);
    printf("other case %d\n", VerQueryValueW(block, L"\\stringfileinfo\\040904b0\\productname", (void **)&s, &len));
    wprint("  value", s);
    printf("missing %d\n", VerQueryValueW(block, L"\\StringFileInfo\\040904B0\\Nothing", (void **)&s, &len));
    char *as = NULL;
    printf("ansi %d len %u\n", VerQueryValueA(block, "\\StringFileInfo\\040904B0\\CompanyName", (void **)&as, &len), len);
    printf("  value \"%s\"\n", as);
    SetLastError(0);
    size = GetFileVersionInfoSizeW(L"C:\\no\\such\\file.exe", &handle);
    printf("missing file %lu, error is set %d\n", size, GetLastError() != 0);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    registry();
    ini();
    version();
    return 0;
}
