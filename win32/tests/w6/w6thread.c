/* W6 thread test: threads, their exit codes, locks, events, TLS (slots and
 * __declspec(thread)), per-thread errno/strtok, _beginthread(ex), suspended
 * threads, and a thread still running when main returns. Output is compared
 * with the same EXE on Windows (expected/w6thread.out). Only main prints,
 * after the threads it waits for are done, so the order is fixed. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <process.h>
#include <windows.h>

static CRITICAL_SECTION cs;
static long counter, atomic_counter;

static DWORD WINAPI adder(void *arg) {
    int n = (int)(INT_PTR)arg;
    for (int i = 0; i < n; i++) {
        EnterCriticalSection(&cs);
        counter++;
        LeaveCriticalSection(&cs);
        InterlockedIncrement(&atomic_counter);
    }
    return (DWORD)n / 1000;
}

static DWORD tls_slot;
static __declspec(thread) int tvar = 5;
static int seen[4];

static DWORD WINAPI tls_user(void *arg) {
    int me = (int)(INT_PTR)arg;
    TlsSetValue(tls_slot, (void *)(INT_PTR)(me * 10));
    tvar += me;
    errno = 30 + me;
    Sleep(20);
    seen[me] = (int)(INT_PTR)TlsGetValue(tls_slot) * 1000 + tvar * 10 + (errno - 30);
    return 0;
}

static HANDLE ping, pong;
static int order[16], order_n;

static DWORD WINAPI ponger(void *arg) {
    (void)arg;
    for (int i = 0; i < 5; i++) {
        WaitForSingleObject(ping, INFINITE);
        order[order_n++] = 100 + i;
        SetEvent(pong);
    }
    return 7;
}

static void nested_exit(void) {
    ExitThread(77);
}

static DWORD WINAPI exits_early(void *arg) {
    (void)arg;
    nested_exit();
    return 1;
}

static unsigned __stdcall beginthreadex_fn(void *arg) {
    return (unsigned)(INT_PTR)arg * 2;
}

static volatile LONG plain_ran;
static void __cdecl beginthread_fn(void *arg) {
    InterlockedExchange(&plain_ran, (LONG)(INT_PTR)arg);
}

static char tok_result[2][64];
static DWORD WINAPI tokenizer(void *arg) {
    int me = (int)(INT_PTR)arg;
    char text[64];
    strcpy(text, me ? "x y z" : "a,b,c,d");
    const char *sep = me ? " " : ",";
    char *out = tok_result[me];
    for (char *t = strtok(text, sep); t; t = strtok(NULL, sep)) {
        strcat(out, t);
        Sleep(5);
    }
    return 0;
}

static DWORD WINAPI summer(void *arg) {
    int k = (int)(INT_PTR)arg;
    DWORD s = 0;
    for (int i = 1; i <= 1000 * k; i++) s += i % 7;
    return s;
}

static DWORD WINAPI forever(void *arg) {
    (void)arg;
    for (;;) Sleep(10);
}

static DWORD WINAPI faulting(void *arg) {
    (void)arg;
    __try {
        *(volatile int *)8 = 1;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return GetExceptionCode();
    }
    return 0;
}

int main(void) {
    DWORD code;

    printf("== critical section ==\n");
    InitializeCriticalSection(&cs);
    HANDLE a = CreateThread(NULL, 0, adder, (void *)100000, 0, NULL);
    HANDLE b = CreateThread(NULL, 0, adder, (void *)50000, 0, NULL);
    HANDLE ab[2] = {a, b};
    WaitForMultipleObjects(2, ab, TRUE, INFINITE);
    GetExitCodeThread(a, &code);
    printf("counter %ld, interlocked %ld, thread a returned %lu", counter, atomic_counter, code);
    GetExitCodeThread(b, &code);
    printf(", b %lu\n", code);
    CloseHandle(a);
    CloseHandle(b);

    printf("== TLS ==\n");
    tls_slot = TlsAlloc();
    TlsSetValue(tls_slot, (void *)1);
    errno = 0;
    HANDLE t[4];
    for (int i = 0; i < 4; i++) t[i] = CreateThread(NULL, 0, tls_user, (void *)(INT_PTR)i, 0, NULL);
    WaitForMultipleObjects(4, t, TRUE, INFINITE);
    for (int i = 0; i < 4; i++) {
        printf("thread %d: %d\n", i, seen[i]);
        CloseHandle(t[i]);
    }
    printf("main: slot %d, tvar %d, errno %d\n", (int)(INT_PTR)TlsGetValue(tls_slot), tvar, errno);

    printf("== events ==\n");
    ping = CreateEventA(NULL, FALSE, FALSE, NULL);
    pong = CreateEventA(NULL, FALSE, FALSE, NULL);
    HANDLE pt = CreateThread(NULL, 0, ponger, NULL, 0, NULL);
    for (int i = 0; i < 5; i++) {
        order[order_n++] = i;
        SetEvent(ping);
        WaitForSingleObject(pong, INFINITE);
    }
    WaitForSingleObject(pt, INFINITE);
    GetExitCodeThread(pt, &code);
    for (int i = 0; i < order_n; i++) printf("%d ", order[i]);
    printf("| ponger returned %lu\n", code);
    CloseHandle(pt);

    printf("== exit codes ==\n");
    HANDLE e = CreateThread(NULL, 0, exits_early, NULL, 0, NULL);
    WaitForSingleObject(e, INFINITE);
    GetExitCodeThread(e, &code);
    printf("ExitThread: %lu\n", code);
    CloseHandle(e);
    HANDLE f = CreateThread(NULL, 0, faulting, NULL, 0, NULL);
    WaitForSingleObject(f, INFINITE);
    GetExitCodeThread(f, &code);
    printf("fault caught in the thread: %08lX\n", code);
    CloseHandle(f);

    printf("== suspended ==\n");
    DWORD tid;
    HANDLE s = CreateThread(NULL, 0, summer, (void *)1, CREATE_SUSPENDED, &tid);
    printf("suspended: wait %lu (timeout %d), id matches %d\n", WaitForSingleObject(s, 100), WAIT_TIMEOUT, GetThreadId(s) == tid);
    GetExitCodeThread(s, &code);
    printf("exit code while suspended: %lu\n", code);
    printf("ResumeThread: %lu\n", ResumeThread(s));
    WaitForSingleObject(s, INFINITE);
    GetExitCodeThread(s, &code);
    printf("after resume: %lu\n", code);
    CloseHandle(s);

    printf("== _beginthread ==\n");
    HANDLE bx = (HANDLE)_beginthreadex(NULL, 0, beginthreadex_fn, (void *)21, 0, NULL);
    WaitForSingleObject(bx, INFINITE);
    GetExitCodeThread(bx, &code);
    printf("_beginthreadex: %lu\n", code);
    CloseHandle(bx);
    _beginthread(beginthread_fn, 0, (void *)9);
    while (!plain_ran) Sleep(1);
    printf("_beginthread ran: %ld\n", plain_ran);

    printf("== strtok per thread ==\n");
    HANDLE tk[2] = {CreateThread(NULL, 0, tokenizer, (void *)0, 0, NULL), CreateThread(NULL, 0, tokenizer, (void *)1, 0, NULL)};
    WaitForMultipleObjects(2, tk, TRUE, INFINITE);
    printf("%s %s\n", tok_result[0], tok_result[1]);
    CloseHandle(tk[0]);
    CloseHandle(tk[1]);

    printf("== many ==\n");
    HANDLE m[8];
    for (int i = 0; i < 8; i++) m[i] = CreateThread(NULL, 0, summer, (void *)(INT_PTR)(i + 1), 0, NULL);
    WaitForMultipleObjects(8, m, TRUE, INFINITE);
    DWORD total = 0;
    for (int i = 0; i < 8; i++) {
        GetExitCodeThread(m[i], &code);
        total += code;
        CloseHandle(m[i]);
    }
    printf("sum %lu\n", total);

    printf("== wait any ==\n");
    HANDLE slow = CreateThread(NULL, 0, forever, NULL, 0, NULL);
    HANDLE quick = CreateThread(NULL, 0, summer, (void *)1, 0, NULL);
    HANDLE both[2] = {slow, quick};
    printf("first done: %lu\n", WaitForMultipleObjects(2, both, FALSE, INFINITE));
    printf("main returns with a thread still running\n");
    return 3;
}
