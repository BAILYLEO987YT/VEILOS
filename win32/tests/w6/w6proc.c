/* W6 child process test: start w6child.exe in various ways, wait for it,
 * read its exit code, talk to it through pipes. Output is compared with
 * the same EXEs on Windows (expected/w6proc.out). */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

/* Start "w6child.exe <args>" with optional std handles; returns the process handle. */
static HANDLE start(const char *args, HANDLE in, HANDLE out, char *env, DWORD flags) {
    char line[512];
    sprintf(line, "w6child.exe %s", args);
    STARTUPINFOA si;
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    if (in || out) {
        si.dwFlags = STARTF_USESTDHANDLES;
        si.hStdInput = in ? in : GetStdHandle(STD_INPUT_HANDLE);
        si.hStdOutput = out ? out : GetStdHandle(STD_OUTPUT_HANDLE);
        si.hStdError = GetStdHandle(STD_ERROR_HANDLE);
    }
    PROCESS_INFORMATION pi;
    fflush(stdout);
    if (!CreateProcessA(NULL, line, NULL, NULL, TRUE, flags, env, NULL, &si, &pi)) {
        printf("CreateProcess(%s) failed: %lu\n", args, GetLastError());
        return NULL;
    }
    CloseHandle(pi.hThread);
    return pi.hProcess;
}

static DWORD finish(HANDLE p) {
    DWORD code = 12345;
    WaitForSingleObject(p, INFINITE);
    GetExitCodeProcess(p, &code);
    CloseHandle(p);
    return code;
}

int main(void) {
    printf("== exit codes ==\n");
    HANDLE p = start("exit 42 300", NULL, NULL, NULL, 0);
    DWORD code;
    GetExitCodeProcess(p, &code);
    printf("while running: %lu (STILL_ACTIVE %d)\n", code, code == STILL_ACTIVE);
    printf("wait: %lu\n", WaitForSingleObject(p, INFINITE));
    GetExitCodeProcess(p, &code);
    printf("exit code: %lu\n", code);
    CloseHandle(p);
    printf("exit 0: %lu\n", finish(start("exit 0", NULL, NULL, NULL, 0)));

    printf("== arguments ==\n");
    finish(start("args one \"two words\" three", NULL, NULL, NULL, 0));

    printf("== environment ==\n");
    SetEnvironmentVariableA("W6VAR", "from parent");
    finish(start("env", NULL, NULL, NULL, 0));
    char block[] = "ONLY=custom block\0W6VAR=replaced\0";
    finish(start("env", NULL, NULL, block, 0));

    printf("== pipe from the child ==\n");
    SECURITY_ATTRIBUTES sa = {sizeof sa, NULL, TRUE};
    HANDLE r, w;
    CreatePipe(&r, &w, &sa, 0);
    SetHandleInformation(r, HANDLE_FLAG_INHERIT, 0);
    p = start("lines 5", NULL, w, NULL, 0);
    CloseHandle(w); /* only the child writes now: end of data when it exits */
    char buf[4096];
    DWORD total = 0, n;
    while (ReadFile(r, buf + total, sizeof buf - 1 - total, &n, NULL) && n > 0) total += n;
    DWORD err = GetLastError();
    buf[total] = 0;
    printf("read %lu bytes (then error %lu):\n%s", total, err, buf);
    CloseHandle(r);
    printf("child exit: %lu\n", finish(p));

    printf("== pipe to the child ==\n");
    CreatePipe(&r, &w, &sa, 0);
    SetHandleInformation(w, HANDLE_FLAG_INHERIT, 0);
    p = start("upper", r, NULL, NULL, 0);
    CloseHandle(r);
    const char *text = "first line\nsecond line\nthird\n";
    DWORD written;
    WriteFile(w, text, (DWORD)strlen(text), &written, NULL);
    CloseHandle(w); /* end of its input */
    printf("child exit: %lu\n", finish(p));

    printf("== crash ==\n");
    printf("crashed child exit: %08lX\n", finish(start("crash", NULL, NULL, NULL, 0)));

    printf("== terminate ==\n");
    p = start("wait", NULL, NULL, NULL, 0);
    printf("wait 200ms: %lu (timeout %d)\n", WaitForSingleObject(p, 200), WAIT_TIMEOUT);
    printf("TerminateProcess: %d\n", TerminateProcess(p, 99));
    printf("terminated exit: %lu\n", finish(p));

    printf("== wait for several ==\n");
    HANDLE two[2] = {start("exit 5 100", NULL, NULL, NULL, 0), start("exit 6 300", NULL, NULL, NULL, 0)};
    DWORD all = WaitForMultipleObjects(2, two, TRUE, INFINITE);
    DWORD c0, c1;
    GetExitCodeProcess(two[0], &c0);
    GetExitCodeProcess(two[1], &c1);
    printf("wait all: %lu, codes %lu %lu\n", all, c0, c1);
    CloseHandle(two[0]);
    CloseHandle(two[1]);

    printf("== missing program ==\n");
    STARTUPINFOA si = {sizeof si};
    PROCESS_INFORMATION pi;
    char missing[] = "no_such_program.exe";
    BOOL ok = CreateProcessA(NULL, missing, NULL, NULL, FALSE, 0, NULL, NULL, &si, &pi);
    printf("CreateProcess: %d, error %lu\n", ok, GetLastError());

    printf("done\n");
    return 0;
}
