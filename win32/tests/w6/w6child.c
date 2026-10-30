/* W6 child process: does what its arguments say (see w6proc.c). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <windows.h>

static LONG WINAPI quiet(EXCEPTION_POINTERS *p) {
    (void)p;
    return EXCEPTION_EXECUTE_HANDLER; /* end with the code, no crash dialog */
}

int main(int argc, char **argv) {
    if (argc < 2) {
        printf("child: no arguments\n");
        return 1;
    }
    if (!strcmp(argv[1], "args")) {
        printf("child: argc=%d", argc);
        for (int i = 2; i < argc; i++) printf(" [%s]", argv[i]);
        printf("\n");
        return 0;
    }
    if (!strcmp(argv[1], "exit")) {
        if (argc > 3) Sleep(atoi(argv[3]));
        return atoi(argv[2]);
    }
    if (!strcmp(argv[1], "env")) {
        const char *v = getenv("W6VAR");
        printf("child: W6VAR=%s ONLY=%s\n", v ? v : "(none)", getenv("ONLY") ? getenv("ONLY") : "(none)");
        return 0;
    }
    if (!strcmp(argv[1], "upper")) {
        /* stdin to stdout, upper-cased, until end of input */
        char line[256];
        int n = 0;
        while (fgets(line, sizeof line, stdin)) {
            for (char *c = line; *c; c++) *c = (char)toupper((unsigned char)*c);
            fputs(line, stdout);
            n++;
        }
        printf("child: %d lines\n", n);
        return n;
    }
    if (!strcmp(argv[1], "lines")) {
        /* numbered lines to stdout (a pipe in the test) */
        int count = atoi(argv[2]);
        for (int i = 1; i <= count; i++) printf("line %d of %d\n", i, count);
        return 0;
    }
    if (!strcmp(argv[1], "cat")) {
        /* a file's contents, with its size */
        FILE *f = fopen(argv[2], "rb");
        if (!f) {
            printf("child: can't open %s\n", argv[2]);
            return 1;
        }
        char data[4096];
        size_t n = fread(data, 1, sizeof data - 1, f);
        data[n] = 0;
        fclose(f);
        printf("child: %s has %zu bytes:\n%s", argv[2], n, data);
        return 0;
    }
    if (!strcmp(argv[1], "crash")) {
        SetUnhandledExceptionFilter(quiet);
        fflush(stdout);
        *(volatile int *)0 = 1;
        return 0;
    }
    if (!strcmp(argv[1], "wait")) {
        Sleep(100000);
        return 0;
    }
    printf("child: unknown command %s\n", argv[1]);
    return 2;
}
