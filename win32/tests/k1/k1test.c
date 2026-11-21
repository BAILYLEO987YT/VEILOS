// K1's Windows program: talks to veiltest.sys the way any Windows program
// talks to a driver, CreateFile("\\.\VeilTest") and DeviceIoControl.
// Prints each check; the exit code is the number that failed (0: all
// passed). Run after DRIVER LOAD /tests/veiltest.sys.
#include <windows.h>
#include <winioctl.h>
#include <stdio.h>
#include <string.h>

#define IOCTL_VEIL_ECHO     CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_ADD      CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_STATS    CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_SELFTEST CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_NEITHER  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_NEITHER, FILE_ANY_ACCESS)

static int failed;

static void check(const char *what, int ok) {
    printf("k1test: %-44s %s\n", what, ok ? "ok" : "FAILED");
    if (!ok) failed++;
}

int main(void) {
    HANDLE h = CreateFileW(L"\\\\.\\VeilTest", GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
    DWORD got = 0;
    check("CreateFile(\\\\.\\VeilTest)", h != INVALID_HANDLE_VALUE);
    if (h == INVALID_HANDLE_VALUE) {
        printf("k1test: error %lu (is veiltest.sys loaded?)\n", GetLastError());
        return 1;
    }

    {
        char in[] = "abcdef", out[16] = { 0 };
        BOOL ok = DeviceIoControl(h, IOCTL_VEIL_ECHO, in, 6, out, sizeof out, &got, NULL);
        check("ECHO reverses (METHOD_BUFFERED)", ok && got == 6 && memcmp(out, "fedcba", 6) == 0);
    }
    {
        ULONG in[2] = { 40, 2 }, sum = 0;
        BOOL ok = DeviceIoControl(h, IOCTL_VEIL_ADD, in, sizeof in, &sum, sizeof sum, &got, NULL);
        check("ADD 40 + 2", ok && got == 4 && sum == 42);
    }
    {
        ULONG sum = 0;
        BOOL ok = DeviceIoControl(h, IOCTL_VEIL_NEITHER, "\x01\x02\x03\x04", 4, &sum, sizeof sum, &got, NULL);
        check("NEITHER sums the caller's own buffer", ok && got == 4 && sum == 10);
    }
    {
        ULONG stats[3] = { 0 };
        BOOL ok = DeviceIoControl(h, IOCTL_VEIL_STATS, NULL, 0, stats, sizeof stats, &got, NULL);
        check("STATS: this handle's create counted", ok && got == 12 && stats[0] >= 1);
        printf("k1test: creates %lu, closes %lu, ioctls %lu\n", stats[0], stats[1], stats[2]);
    }
    {
        ULONG mask = 0;
        BOOL ok = DeviceIoControl(h, IOCTL_VEIL_SELFTEST, NULL, 0, &mask, sizeof mask, &got, NULL);
        check("driver self-test: registry", ok && (mask & 0x01));
        check("driver self-test: system thread", ok && (mask & 0x02));
        check("driver self-test: DPC at DISPATCH_LEVEL", ok && (mask & 0x04));
        check("driver self-test: periodic timer + DPC", ok && (mask & 0x08));
        check("driver self-test: work item", ok && (mask & 0x10));
        check("driver SEH: page fault into __except", ok && (mask & 0x20));
        check("driver SEH: ExRaiseStatus through 2 frames", ok && (mask & 0x40));
        check("driver SEH: both __finally blocks ran", ok && (mask & 0x80));
        check("driver SEH: ProbeForRead", ok && (mask & 0x100));
        check("driver SEH: EXCEPTION_CONTINUE_EXECUTION", ok && (mask & 0x200));
        check("driver memory: contiguous below 4 GiB", ok && (mask & 0x400));
        check("driver memory: MmMapIoSpace of it", ok && (mask & 0x800));
        check("driver memory: MDL over pool", ok && (mask & 0x1000));
        check("driver memory: pages for an MDL, mapped", ok && (mask & 0x2000));
        check("driver CRT: sprintf_s / swprintf_s", ok && (mask & 0x4000));
        check("driver Rtl: counted strings, numbers", ok && (mask & 0x8000));
        check("driver Rtl: bitmaps", ok && (mask & 0x10000));
        check("driver Rtl: generic tables", ok && (mask & 0x20000));
        check("driver Ex: resources, fast/guarded mutexes", ok && (mask & 0x40000));
        check("driver Ex: lookaside lists, SLists", ok && (mask & 0x80000));
        check("driver Rtl: time fields, RtlQueryRegistryValues", ok && (mask & 0x100000));
        check("driver Ex: timer, callback object", ok && (mask & 0x200000));
    }
    {
        // METHOD_OUT_DIRECT: the driver writes straight into this buffer
        // (three pages, starting partway into the first) through an MDL.
        UCHAR *mem = (UCHAR *)VirtualAlloc(NULL, 4 * 4096, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        UCHAR seed = 0x21;
        int good = mem != NULL;
        BOOL ok = FALSE;
        if (mem) {
            ok = DeviceIoControl(h, CTL_CODE(FILE_DEVICE_UNKNOWN, 0x806, METHOD_OUT_DIRECT, FILE_ANY_ACCESS), &seed, 1, mem + 100, 9000, &got, NULL);
            for (ULONG i = 0; i < 9000 && good; i++) good = mem[100 + i] == (UCHAR)(seed + i * 7);
            good = good && mem[99] == 0 && mem[9100] == 0;
        }
        check("METHOD_OUT_DIRECT across 3 pages", ok && got == 9000 && good);
    }
    {
        ULONG mask = 0;
        BOOL ok = DeviceIoControl(h, CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_ANY_ACCESS), NULL, 0, &mask, sizeof mask, &got, NULL);
        check("in a request: wait on a system thread", ok && (mask & 0x01));
        check("in a request: wait on a timer", ok && (mask & 0x02));
        check("in a request: KeDelayExecutionThread", ok && (mask & 0x04));
        check("in a request: wait for a work item", ok && (mask & 0x08));
        check("in a request: wait for a timer's DPC", ok && (mask & 0x10));
    }
    {
        BOOL ok = DeviceIoControl(h, CTL_CODE(FILE_DEVICE_UNKNOWN, 0x8FF, METHOD_BUFFERED, FILE_ANY_ACCESS), NULL, 0, NULL, 0, &got, NULL);
        check("unknown code fails (ERROR_INVALID_FUNCTION)", !ok && GetLastError() == ERROR_INVALID_FUNCTION);
    }
    check("CloseHandle", CloseHandle(h));
    printf("k1test: %d failed\n", failed);
    return failed;
}
