/* W5 C runtime test, built /MD against the UCRT (vcruntime140 + api-ms-win-crt-*).
 * Its output is compared with the same EXE run on Windows (expected/w5crt.txt).
 * Everything printed must be deterministic: no times, paths or addresses. */
#define _CRT_SECURE_NO_WARNINGS
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <ctype.h>
#include <math.h>
#include <time.h>
#include <errno.h>
#include <setjmp.h>
#include <locale.h>

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

static void bye1(void) { printf("atexit 1 (runs last)\n"); }
static void bye2(void) { printf("atexit 2 (runs first)\n"); }

static jmp_buf jb;
static void jump(int v) { longjmp(jb, v); }

static void section(const char *name) { printf("\n== %s ==\n", name); }

int main(int argc, char **argv) {
    atexit(bye1);
    atexit(bye2);

    section("args");
    printf("argc=%d\n", argc);
    for (int i = 1; i < argc; i++) printf("argv[%d]=[%s]\n", i, argv[i]);

    section("integers");
    printf("[%d] [%i] [%u] [%5d] [%-5d|] [%05d] [%+d] [% d]\n", 42, -42, 3000000000u, 7, 7, -7, 7, 7);
    printf("[%x] [%X] [%#x] [%#o] [%o] [%#X]\n", 255, 255, 255, 8, 8, 0);
    printf("[%lld] [%llu] [%I64d] [%zd] [%hd] [%hhd] [%hu]\n", -9223372036854775807LL - 1, 18446744073709551615ULL,
           1234567890123LL, (size_t)99, (short)-2, (char)-3, (unsigned short)65535);
    printf("[%.3d] [%.0d] [%8.3d] [%-8.3x|] [%*d] [%-*d|] [%.*d]\n", 5, 0, 5, 10, 6, 1, 6, 1, 4, 9);
    printf("[%p]\n", (void *)0x1234ABCD);

    section("floats");
    printf("[%f] [%.2f] [%10.3f] [%-10.1f|] [%+.0f] [%#.0f] [%010.2f]\n", 3.14159, 2.675, -1.5, 2.25, 2.5, 3.0, -3.14159);
    printf("[%e] [%.3E] [%.0e] [%#.0e] [%12.4e]\n", 12345.678, 0.000123456, 5e10, 5e10, -1e-300);
    printf("[%g] [%g] [%g] [%g] [%G] [%#g] [%.3g] [%.10g]\n", 100000.0, 1000000.0, 0.0001, 0.00001, 1e-10, 1.5, 3.14159, 1.0 / 3);
    printf("[%a] [%A] [%.2a] [%a]\n", 1.0, -0.5, 1.999, 0.0);
    printf("[%f] [%f] [%f] [%e] [%5.1f]\n", 1e300 * 1e300, -1e300 * 1e300, 0.0 / 0.0 * 0.0, 1e300 * 1e300, -0.0);
    printf("[%.20f] [%.15g] [%.17g]\n", 0.1, 0.1, 0.1);
    printf("[%f] [%.0f] [%.1f]\n", 1e20, 0.5, 0.05);

    section("strings and chars");
    printf("[%s] [%10s] [%-10s|] [%.3s] [%c] [%3c] [%%] [%ls] [%S] [%5.2s]\n", "hello", "right", "left", "truncate", 'x', 'y',
           L"wide", L"Wide", "abc");
    printf("[%s]\n", (char *)NULL);
    wchar_t wbuf[64];
    swprintf(wbuf, 64, L"%d %s %hs %c|", 12, L"wide", "narrow", L'W');
    printf("swprintf: %ls\n", wbuf);

    section("snprintf");
    char buf[16];
    int n = snprintf(buf, sizeof buf, "%s", "0123456789abcdefghij");
    printf("snprintf -> %d [%s]\n", n, buf);
    n = snprintf(NULL, 0, "%d-%d", 123, 456);
    printf("snprintf(NULL) -> %d\n", n);
    n = sprintf(buf, "%05.1f", 9.96);
    printf("sprintf -> %d [%s]\n", n, buf);
    n = _snprintf(buf, 4, "abcdef");
    printf("_snprintf -> %d [%.4s]\n", n, buf);
    n = sprintf_s(buf, sizeof buf, "%s", "fits");
    printf("sprintf_s -> %d [%s]\n", n, buf);
    n = _snprintf_s(buf, sizeof buf, _TRUNCATE, "%s", "this is far too long");
    printf("_snprintf_s -> %d [%s]\n", n, buf);

    section("scanf");
    int a = 0, b = 0;
    unsigned x = 0;
    double d = 0;
    float f = 0;
    char word[32] = "", rest[32] = "";
    n = sscanf("  42 -17 ff 3.5e2 1.25 hello, world", "%d %i %x %lf %f %31s %31[^\n]", &a, &b, &x, &d, &f, word, rest);
    printf("sscanf -> %d: %d %d %u %g %g [%s] [%s]\n", n, a, b, x, d, f, word, rest);
    n = sscanf("0x1A 017 9", "%i %i %i", &a, &b, &x);
    printf("sscanf %%i -> %d: %d %d %u\n", n, a, b, x);
    int consumed = 0;
    n = sscanf("abc123def", "%[a-z]%d%n", word, &a, &consumed);
    printf("sscanf set -> %d: [%s] %d consumed=%d\n", n, word, a, consumed);
    n = sscanf("", "%d", &a);
    printf("sscanf empty -> %d\n", n);
    n = sscanf("x", "%d", &a);
    printf("sscanf mismatch -> %d\n", n);
    char c3[4] = {0};
    n = sscanf("xyz", "%3c", c3);
    printf("sscanf %%3c -> %d [%s]\n", n, c3);
    n = sscanf_s("token 5", "%s %d", word, (unsigned)sizeof word, &a);
    printf("sscanf_s -> %d [%s] %d\n", n, word, a);
    wchar_t wword[16];
    n = swscanf(L"wide 77", L"%15ls %d", wword, &a);
    printf("swscanf -> %d [%ls] %d\n", n, wword, a);

    section("conversions");
    char *end;
    printf("strtol: %ld %ld %ld\n", strtol("  -123xyz", &end, 10), strtol("0x7fffffff", NULL, 0), strtol("777", NULL, 8));
    printf("end=[%s]\n", end);
    errno = 0;
    long big = strtol("99999999999", NULL, 10);
    printf("overflow: %ld errno=%d\n", big, errno);
    printf("strtoul: %lu %lu\n", strtoul("4294967295", NULL, 10), strtoul("-1", NULL, 10));
    printf("strtoull: %llu strtoll: %lld\n", strtoull("18446744073709551615", NULL, 10), strtoll("-42", NULL, 0));
    printf("strtod: %g %g %g %g %g\n", strtod("1e-5", NULL), strtod("  .5", NULL), strtod("-inf", NULL), strtod("0x1.8p3", NULL), strtod("12abc", &end));
    printf("end=[%s] atof=%g atoi=%d atol=%ld\n", end, atof("2.5e3"), atoi("  -88z"), atol("123"));
    char num[40];
    printf("_itoa: %s ", _itoa(-255, num, 16));
    printf("%s ", _itoa(-255, num, 10));
    printf("%s ", _ultoa(4000000000u, num, 10));
    printf("%s\n", _i64toa(-1, num, 2));
    wchar_t wnum[40];
    printf("wcstol: %ld _wtoi: %d\n", wcstol(L"-0x10", NULL, 16), _wtoi(L"321"));
    printf("_itow: %ls\n", _itow(1234, wnum, 10));
    char mb[16];
    /* ASCII only: VEILOS's narrow strings are UTF-8, Windows' "C" locale is Latin-1. */
    size_t len = wcstombs(mb, L"hello", sizeof mb);
    printf("wcstombs -> %zu bytes:", len);
    for (size_t i = 0; i < len; i++) printf(" %02x", (unsigned char)mb[i]);
    printf("\n");

    section("strings");
    char s1[64] = "Hello";
    strcat(s1, ", World");
    printf("%s len=%zu cmp=%d,%d,%d icmp=%d ncmp=%d\n", s1, strlen(s1), strcmp("a", "b"), strcmp("b", "a"), strcmp("x", "x"),
           _stricmp("HeLLo", "hello"), strncmp("abcd", "abef", 2));
    printf("strchr=[%s] strrchr=[%s] strstr=[%s] spn=%zu cspn=%zu pbrk=[%s]\n", strchr(s1, 'o'), strrchr(s1, 'o'), strstr(s1, "Wor"),
           strspn("aabbcd", "ab"), strcspn("hello", "lo"), strpbrk("hello", "lx"));
    char toks[] = "a,b,,c";
    for (char *t = strtok(toks, ","); t; t = strtok(NULL, ",")) printf("tok=[%s] ", t);
    printf("\n");
    char up[] = "MiXeD";
    printf("_strupr=[%s] ", _strupr(up));
    printf("_strlwr=[%s] ", _strlwr(up));
    printf("_strrev=[%s]\n", _strrev(up));
    char *dup = _strdup("copy");
    printf("_strdup=[%s] memcmp=%d memchr=[%s]\n", dup, memcmp("abc", "abd", 3) < 0, (char *)memchr("find me", 'm', 7));
    free(dup);
    char small[4];
    printf("strcpy_s too long -> %d [%s]\n", strncpy_s(small, sizeof small, "abcdef", _TRUNCATE), small);
    printf("wcslen=%zu wcscmp=%d _wcsicmp=%d wcschr=[%ls] wcsstr=[%ls]\n", wcslen(L"wide"), wcscmp(L"a", L"a"), _wcsicmp(L"ABC", L"abc"),
           wcschr(L"wide", L'd'), wcsstr(L"haystack", L"st"));
    /* strerror shares one buffer on Windows: one call per printf. */
    printf("strerror(2)=[%s] ", strerror(2));
    printf("strerror(22)=[%s]\n", strerror(22));

    section("ctype");
    for (const char *p = "aZ5 \t!\x7f"; *p; p++)
        printf("%02x:%d%d%d%d%d%d%d ", (unsigned char)*p, !!isalpha(*p), !!isdigit(*p), !!isspace(*p), !!ispunct(*p), !!isupper(*p),
               !!isprint(*p), !!iscntrl(*p));
    printf("\ntoupper=%c tolower=%c towupper=%04x\n", toupper('q'), tolower('Q'), (unsigned)towupper(L'é'));

    section("math");
    printf("%.6f %.6f %.6f %.6f %.6f\n", sqrt(2.0), sin(1.0), cos(1.0), atan2(1.0, -1.0), exp(1.0));
    printf("%.6f %.6f %.6f %.6f %.6f\n", log(10.0), log10(1000.0), pow(2.0, 0.5), fmod(10.5, 3.0), hypot(3.0, 4.0));
    printf("%g %g %g %g %g %g\n", floor(-2.5), ceil(-2.5), round(-2.5), trunc(-2.7), fabs(-3.0), ldexp(1.0, 10));
    int ex;
    double fr = frexp(48.0, &ex);
    double ip;
    double fp = modf(-3.75, &ip);
    printf("frexp=%g,%d modf=%g,%g isnan=%d isinf=%d signbit=%d fpclassify(1)=%d\n", fr, ex, fp, ip, isnan(NAN), isinf(-INFINITY),
           signbit(-0.0) != 0, fpclassify(1.0));
    printf("abs=%d labs=%ld llabs=%lld div=%d,%d\n", abs(-5), labs(-6L), llabs(-7LL), div(17, 5).quot, div(17, 5).rem);

    section("qsort, bsearch, rand");
    int arr[] = {5, 3, 9, 1, 7, 3, 8, 2, 6, 4, 0, 11, 10};
    int count = sizeof arr / sizeof arr[0];
    qsort(arr, count, sizeof arr[0], cmp_int);
    for (int i = 0; i < count; i++) printf("%d ", arr[i]);
    int key = 7;
    int *found = bsearch(&key, arr, count, sizeof arr[0], cmp_int);
    printf("| found 7 at %d\n", found ? (int)(found - arr) : -1);
    srand(12345);
    for (int i = 0; i < 5; i++) printf("%d ", rand());
    printf("\n");

    section("time");
    time_t t = 951782400; /* 2000-02-29 00:00:00 UTC */
    struct tm *g = gmtime(&t);
    printf("gmtime: %d-%02d-%02d %02d:%02d:%02d wday=%d yday=%d\n", g->tm_year + 1900, g->tm_mon + 1, g->tm_mday, g->tm_hour, g->tm_min,
           g->tm_sec, g->tm_wday, g->tm_yday);
    char when[64];
    strftime(when, sizeof when, "%Y-%m-%d %H:%M:%S %a %b %j %A %B", g);
    printf("strftime: %s\n", when);
    struct tm in = {0};
    in.tm_year = 124;
    in.tm_mon = 13; /* normalises to 2025-02 */
    in.tm_mday = 1;
    long long made = (long long)_mkgmtime(&in);
    printf("_mkgmtime: %lld -> month %d year %d\n", made, in.tm_mon, in.tm_year);
    printf("asctime: %s", asctime(g));
    printf("time() > 2020: %d\n", time(NULL) > 1577836800);

    section("files");
    FILE *fp1 = fopen("w5tmp.txt", "w");
    printf("fopen w: %s\n", fp1 ? "ok" : "failed");
    fprintf(fp1, "line one\nline %d\n", 2);
    fputs("third\n", fp1);
    fputc('X', fp1);
    fclose(fp1);
    FILE *fp2 = fopen("w5tmp.txt", "rb");
    fseek(fp2, 0, SEEK_END);
    printf("binary size: %ld (CRLF)\n", ftell(fp2));
    fclose(fp2);
    FILE *fp3 = fopen("w5tmp.txt", "r");
    char line[64];
    while (fgets(line, sizeof line, fp3)) printf("read: [%.*s] ftell=%ld\n", (int)strcspn(line, "\n"), line, ftell(fp3));
    printf("feof=%d\n", feof(fp3) != 0);
    rewind(fp3);
    int c = fgetc(fp3);
    ungetc(c, fp3);
    fscanf(fp3, "%s", word);
    printf("after rewind: %c [%s]\n", c, word);
    fclose(fp3);
    FILE *fp4 = fopen("w5tmp.txt", "a+");
    fputs("appended\n", fp4);
    fseek(fp4, 0, SEEK_SET);
    n = 0;
    while (fgets(line, sizeof line, fp4)) n++;
    printf("lines after append: %d\n", n);
    fclose(fp4);
    int r1 = remove("w5tmp.txt");
    errno = 0;
    int r2 = remove("w5tmp.txt");
    printf("remove: %d, again: %d errno=%d\n", r1, r2, errno);
    FILE *missing = fopen("no_such_file.txt", "r");
    printf("fopen missing: %s errno=%d\n", missing ? "found" : "NULL", errno);

    section("environment");
    _putenv("W5_TEST=value one");
    printf("getenv: [%s]\n", getenv("W5_TEST"));
    printf("getenv case: [%s]\n", getenv("w5_test"));
    _putenv("W5_TEST=");
    printf("after unset: %s\n", getenv("W5_TEST") ? "still set" : "NULL");
    printf("PATH set: %d\n", getenv("PATH") != NULL);

    section("setjmp");
    volatile int stage = 0;
    int r = setjmp(jb);
    printf("setjmp returned %d (stage %d)\n", r, stage);
    if (stage++ == 0) jump(7);

    section("locale");
    printf("setlocale: %s decimal_point=[%s]\n", setlocale(LC_ALL, NULL), localeconv()->decimal_point);

    section("stderr");
    fprintf(stderr, "to stderr\n");
    fflush(stderr);
    errno = ENOENT;
    perror("perror");

    section("end");
    printf("exit code 3 follows\n");
    return 3;
}
