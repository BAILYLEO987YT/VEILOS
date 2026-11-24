// K2's Windows program: opens both edu devices k2edu.sys runs
// (\\.\VeilEdu0 and \\.\VeilEdu1), reads each one's test results, and
// checks the two stay apart: their own resources, vectors and interrupts.
// The exit code is the number of failed checks. Run after DRIVER LOAD
// /tests/k2edu.sys in a VM with two edu devices.
#include <windows.h>
#include <winioctl.h>
#include <stdio.h>

#define IOCTL_K2_RESULT CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_K2_RAISE  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_BUFFERED, FILE_ANY_ACCESS)

typedef struct {
    ULONG Mask, Index, Bus, Address, Vector, Irql;
    ULONG IsrCount, DpcCount, LatencyUs, TimerTicks, OtherIndex, Factorial;
} K2_RESULT;

static const char *names[] = {
    "attached above its PDO", "device properties", "device and driver registry keys", "device interface",
    "resources: memory + message interrupt", "MmMapIoSpaceEx, edu registers", "bus interface: config space",
    "IoConnectInterruptEx (message-based)", "8 interrupts, ISR -> DPC", "DPC within 500 us of its ISR",
    "factorial, finished by interrupt", "KeSynchronizeExecution at DIRQL", "remove lock", "IRP_MN_READ_CONFIG",
    "PoRequestPowerIrp D0", "IoTimer ticking", "IOCTL to the other device", "IoCancelIrp",
    "PnP interface notification", "error log, named event, ObOpenObjectByPointer", "stack limits, driver path, keys",
    "device property data",
};

static int failed;

static void check(const char *what, int ok) {
    printf("k2test: %-50s %s\n", what, ok ? "ok" : "FAILED");
    if (!ok) failed++;
}

static int result(HANDLE h, K2_RESULT *r) {
    DWORD got = 0;
    return DeviceIoControl(h, IOCTL_K2_RESULT, NULL, 0, r, sizeof *r, &got, NULL) && got == sizeof *r;
}

int main(void) {
    HANDLE h[2];
    K2_RESULT r[2], after[2];
    int i, b;
    char what[96];
    for (i = 0; i < 2; i++) {
        wchar_t name[32];
        swprintf(name, 32, L"\\\\.\\VeilEdu%d", i);
        h[i] = CreateFileW(name, GENERIC_READ | GENERIC_WRITE, 0, NULL, OPEN_EXISTING, 0, NULL);
        sprintf(what, "CreateFile(\\\\.\\VeilEdu%d)", i);
        check(what, h[i] != INVALID_HANDLE_VALUE);
        if (h[i] == INVALID_HANDLE_VALUE) {
            printf("k2test: error %lu (is k2edu.sys loaded, with two edu devices?)\n", GetLastError());
            return 1;
        }
    }
    for (i = 0; i < 2; i++) {
        memset(&r[i], 0, sizeof r[i]);
        check(i ? "results from device 1" : "results from device 0", result(h[i], &r[i]));
        printf("k2test: device %lu at %02lx:%02lx.%lu: tests %06lx, vector %lx at IRQL %lu, %lu interrupts, %lu DPCs, DPC %lu us after ISR, timer %lu, 10! = %lu\n",
               r[i].Index, r[i].Bus, r[i].Address >> 16, r[i].Address & 0xFFFF, r[i].Mask, r[i].Vector, r[i].Irql, r[i].IsrCount,
               r[i].DpcCount, r[i].LatencyUs, r[i].TimerTicks, r[i].Factorial);
        for (b = 0; b < (int)(sizeof names / sizeof names[0]); b++) {
            sprintf(what, "dev %d: %s", i, names[b]);
            check(what, (r[i].Mask >> b) & 1);
        }
    }
    check("devices have different indexes", r[0].Index != r[1].Index);
    check("devices are different PCI functions", r[0].Bus != r[1].Bus || r[0].Address != r[1].Address);
    check("devices have different vectors", r[0].Vector && r[1].Vector && r[0].Vector != r[1].Vector);
    {
        // Interrupts raised on device 0 reach device 0's ISR only.
        ULONG n = 5, ok = 0;
        DWORD got = 0;
        BOOL sent = DeviceIoControl(h[0], IOCTL_K2_RAISE, &n, sizeof n, &ok, sizeof ok, &got, NULL);
        check("5 more interrupts on device 0", sent && ok == 5);
        if (result(h[0], &after[0]) && result(h[1], &after[1])) {
            printf("k2test: interrupts now %lu and %lu (were %lu and %lu)\n", after[0].IsrCount, after[1].IsrCount, r[0].IsrCount, r[1].IsrCount);
            check("device 0 took them", after[0].IsrCount >= r[0].IsrCount + 5);
            check("device 1 didn't", after[1].IsrCount == r[1].IsrCount);
        } else {
            check("results after raising", 0);
        }
    }
    for (i = 0; i < 2; i++) CloseHandle(h[i]);
    printf("k2test: %d failed\n", failed);
    return failed;
}
