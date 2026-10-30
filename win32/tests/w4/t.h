// Shared by the W4 tests: output to the console and PASS/FAIL checks. No C
// runtime, so the few memory routines the compiler may call are here too.
#include <windows.h>

void *memset(void *d, int c, size_t n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, size_t n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}
#pragma function(memset)
#pragma function(memcpy)

static int failures = 0;

static DWORD slen(const char *s) {
    DWORD n = 0;
    while (s[n]) n++;
    return n;
}

static void out(const char *s) {
    DWORD n;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, slen(s), &n, 0);
}

static void out_num(long long v) {
    char buf[24];
    int i = 23, neg = v < 0;
    unsigned long long u = neg ? (unsigned long long)-v : (unsigned long long)v;
    buf[i] = 0;
    do { buf[--i] = (char)('0' + u % 10); u /= 10; } while (u);
    if (neg) buf[--i] = '-';
    out(buf + i);
}

static void check(const char *name, int ok) {
    out(ok ? "PASS " : "FAIL ");
    out(name);
    out("\r\n");
    if (!ok) failures++;
}

/// Like check, with a number shown (an error code, a count...).
static void check_n(const char *name, int ok, long long n) {
    out(ok ? "PASS " : "FAIL ");
    out(name);
    out(" (");
    out_num(n);
    out(")\r\n");
    if (!ok) failures++;
}

static int seq(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}

static int wseq(const WCHAR *a, const WCHAR *b) {
    while (*a && *a == *b) { a++; b++; }
    return *a == *b;
}

static void done(const char *name) {
    out(name);
    out(failures ? ": FAILURES " : ": all passed ");
    out_num(failures);
    out("\r\n");
    ExitProcess(failures);
}
