// W4 test: time, Sleep, events, waits, critical sections, SRW locks, TLS,
// one-time initialisation.
#include "t.h"

static int once_runs = 0;
static BOOL CALLBACK once_fn(PINIT_ONCE once, PVOID param, PVOID *ctx) {
    once_runs++;
    return TRUE;
}

void start(void) {
    DWORD t0 = GetTickCount();
    Sleep(200);
    DWORD slept = GetTickCount() - t0;
    check_n("Sleep(200) takes 200 ms or a little more", slept >= 195 && slept < 400, slept);

    LARGE_INTEGER f, a, b;
    QueryPerformanceFrequency(&f);
    QueryPerformanceCounter(&a);
    Sleep(10);
    QueryPerformanceCounter(&b);
    check_n("performance counter: frequency (MHz)", f.QuadPart > 1000000, f.QuadPart / 1000000);
    check("performance counter advances", b.QuadPart > a.QuadPart);
    check("GetTickCount64 agrees", GetTickCount64() >= GetTickCount());

    SYSTEMTIME st;
    GetSystemTime(&st);
    out("system time: ");
    out_num(st.wYear); out("-"); out_num(st.wMonth); out("-"); out_num(st.wDay); out(" ");
    out_num(st.wHour); out(":"); out_num(st.wMinute); out(":"); out_num(st.wSecond); out(" UTC\r\n");
    check("system time is sensible", st.wYear >= 2024 && st.wMonth >= 1 && st.wMonth <= 12);
    FILETIME ft, ft2;
    SYSTEMTIME back;
    SystemTimeToFileTime(&st, &ft);
    FileTimeToSystemTime(&ft, &back);
    SystemTimeToFileTime(&back, &ft2);
    check("SYSTEMTIME <-> FILETIME round trip", ft.dwLowDateTime == ft2.dwLowDateTime && ft.dwHighDateTime == ft2.dwHighDateTime && back.wDay == st.wDay);
    SYSTEMTIME epoch = { 1601, 1, 1, 1, 0, 0, 0, 0 };
    SystemTimeToFileTime(&epoch, &ft);
    check("1601-01-01 is FILETIME 0", ft.dwLowDateTime == 0 && ft.dwHighDateTime == 0);

    HANDLE ev = CreateEventA(0, TRUE, FALSE, 0);
    t0 = GetTickCount();
    DWORD r = WaitForSingleObject(ev, 100);
    DWORD waited = GetTickCount() - t0;
    check_n("wait on an unset event times out after 100 ms", r == WAIT_TIMEOUT && waited >= 95, waited);
    SetEvent(ev);
    check("set manual event: wait returns at once", WaitForSingleObject(ev, 0) == WAIT_OBJECT_0);
    check("manual event stays set", WaitForSingleObject(ev, 0) == WAIT_OBJECT_0);
    ResetEvent(ev);
    check("ResetEvent", WaitForSingleObject(ev, 0) == WAIT_TIMEOUT);
    CloseHandle(ev);
    HANDLE auto_ev = CreateEventA(0, FALSE, TRUE, 0);
    check("auto-reset event: first wait succeeds", WaitForSingleObject(auto_ev, 0) == WAIT_OBJECT_0);
    check("auto-reset event: then it's reset", WaitForSingleObject(auto_ev, 0) == WAIT_TIMEOUT);
    CloseHandle(auto_ev);

    CRITICAL_SECTION cs;
    InitializeCriticalSection(&cs);
    EnterCriticalSection(&cs);
    EnterCriticalSection(&cs);
    check("critical section is recursive", cs.RecursionCount == 2);
    LeaveCriticalSection(&cs);
    LeaveCriticalSection(&cs);
    check("TryEnterCriticalSection when free", TryEnterCriticalSection(&cs));
    LeaveCriticalSection(&cs);
    DeleteCriticalSection(&cs);

    SRWLOCK lock = SRWLOCK_INIT;
    AcquireSRWLockShared(&lock);
    check("SRW: a second reader gets in", TryAcquireSRWLockShared(&lock));
    check("SRW: a writer doesn't while readers hold it", !TryAcquireSRWLockExclusive(&lock));
    ReleaseSRWLockShared(&lock);
    ReleaseSRWLockShared(&lock);
    check("SRW: a writer gets in once they leave", TryAcquireSRWLockExclusive(&lock));
    ReleaseSRWLockExclusive(&lock);

    DWORD slot = TlsAlloc();
    check("TlsAlloc", slot != TLS_OUT_OF_INDEXES);
    check("TlsGetValue of a new slot is 0", TlsGetValue(slot) == 0);
    TlsSetValue(slot, (LPVOID)0x1234);
    check("TlsSetValue / TlsGetValue", TlsGetValue(slot) == (LPVOID)0x1234);
    check("TlsFree", TlsFree(slot));

    INIT_ONCE once = INIT_ONCE_STATIC_INIT;
    InitOnceExecuteOnce(&once, once_fn, 0, 0);
    InitOnceExecuteOnce(&once, once_fn, 0, 0);
    check_n("InitOnceExecuteOnce runs once", once_runs == 1, once_runs);

    done("w4sync");
}
