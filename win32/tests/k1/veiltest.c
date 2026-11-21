// K1: a small WDM driver for VEILOS's Windows kernel-driver host
// (WDDM_DESIGN.md, stage W1), built against the WDK's own headers.
//
// DriverEntry exercises pool, strings, spinlocks, events, version and time,
// creates \Device\VeilTest with the link \DosDevices\VeilTest, and answers:
//   IOCTL_VEIL_ECHO  (METHOD_BUFFERED): the input bytes reversed
//   IOCTL_VEIL_ADD   (METHOD_BUFFERED): two ULONGs in, their sum out
//   IOCTL_VEIL_STATS (METHOD_BUFFERED): counters: creates, closes, ioctls
// DriverUnload removes the link and the device and frees the pool.
#include <ntddk.h>

// Exported by ntoskrnl, but only declared in the user-mode headers.
NTSYSAPI VOID NTAPI RtlRaiseException(PEXCEPTION_RECORD ExceptionRecord);

#define IOCTL_VEIL_ECHO  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_ADD   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_STATS CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_SELFTEST CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_NEITHER  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_NEITHER, FILE_ANY_ACCESS)
// Waits inside the request itself (a program's system call): see WaitTest.
#define IOCTL_VEIL_WAITTEST CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_ANY_ACCESS)

// Self-test results (bits), run in DriverEntry and reported by SELFTEST.
#define T_REGISTRY 0x01
#define T_THREAD   0x02
#define T_DPC      0x04
#define T_TIMER    0x08
#define T_WORKITEM 0x10
#define T_SEH_FAULT    0x20
#define T_SEH_RAISE    0x40
#define T_SEH_FINALLY  0x80
#define T_SEH_PROBE    0x100
#define T_SEH_CONTINUE 0x200
#define T_ALL      0x3FF

typedef struct _VEIL_EXT {
    KSPIN_LOCK Lock;
    ULONG Creates, Closes, Ioctls;
} VEIL_EXT;

static PVOID g_scratch;
static UNICODE_STRING g_link;
static ULONG g_passed;

static KEVENT g_go, g_done;
static volatile LONG g_thread_value, g_ticks;
static volatile KIRQL g_dpc_irql = 0xFF;
static KDPC g_dpc, g_timer_dpc;
static KTIMER g_timer;

DRIVER_INITIALIZE DriverEntry;
DRIVER_UNLOAD VeilUnload;
_Dispatch_type_(IRP_MJ_CREATE) _Dispatch_type_(IRP_MJ_CLOSE) DRIVER_DISPATCH VeilCreateClose;
_Dispatch_type_(IRP_MJ_DEVICE_CONTROL) DRIVER_DISPATCH VeilIoctl;

NTSTATUS VeilCreateClose(PDEVICE_OBJECT dev, PIRP irp) {
    VEIL_EXT *x = (VEIL_EXT *)dev->DeviceExtension;
    KIRQL old;
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(irp);
    KeAcquireSpinLock(&x->Lock, &old);
    if (sl->MajorFunction == IRP_MJ_CREATE) x->Creates++; else x->Closes++;
    KeReleaseSpinLock(&x->Lock, old);
    irp->IoStatus.Status = STATUS_SUCCESS;
    irp->IoStatus.Information = 0;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return STATUS_SUCCESS;
}

static ULONG WaitTest(PDEVICE_OBJECT dev);

NTSTATUS VeilIoctl(PDEVICE_OBJECT dev, PIRP irp) {
    VEIL_EXT *x = (VEIL_EXT *)dev->DeviceExtension;
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(irp);
    ULONG code = sl->Parameters.DeviceIoControl.IoControlCode;
    ULONG in = sl->Parameters.DeviceIoControl.InputBufferLength;
    ULONG out = sl->Parameters.DeviceIoControl.OutputBufferLength;
    UCHAR *buf = (UCHAR *)irp->AssociatedIrp.SystemBuffer;
    NTSTATUS st = STATUS_SUCCESS;
    ULONG_PTR info = 0;
    KIRQL old;

    KeAcquireSpinLock(&x->Lock, &old);
    x->Ioctls++;
    KeReleaseSpinLock(&x->Lock, old);

    switch (code) {
    case IOCTL_VEIL_ECHO:
        if (out < in) { st = STATUS_BUFFER_TOO_SMALL; break; }
        for (ULONG i = 0; i < in / 2; i++) {
            UCHAR t = buf[i]; buf[i] = buf[in - 1 - i]; buf[in - 1 - i] = t;
        }
        info = in;
        break;
    case IOCTL_VEIL_ADD: {
        ULONG a, b;
        if (in < 2 * sizeof(ULONG) || out < sizeof(ULONG)) { st = STATUS_BUFFER_TOO_SMALL; break; }
        RtlCopyMemory(&a, buf, sizeof a);
        RtlCopyMemory(&b, buf + 4, sizeof b);
        a += b;
        RtlCopyMemory(buf, &a, sizeof a);
        info = sizeof a;
        break;
    }
    case IOCTL_VEIL_SELFTEST:
        if (out < sizeof(ULONG)) { st = STATUS_BUFFER_TOO_SMALL; break; }
        RtlCopyMemory(buf, &g_passed, sizeof g_passed);
        info = sizeof g_passed;
        break;
    case IOCTL_VEIL_WAITTEST: {
        ULONG mask;
        if (out < sizeof(ULONG)) { st = STATUS_BUFFER_TOO_SMALL; break; }
        mask = WaitTest(dev);
        RtlCopyMemory(buf, &mask, sizeof mask);
        info = sizeof mask;
        break;
    }
    case IOCTL_VEIL_NEITHER: {
        // The caller's own buffers: the sum of its input bytes, as a ULONG.
        UCHAR *src = (UCHAR *)sl->Parameters.DeviceIoControl.Type3InputBuffer;
        ULONG sum = 0;
        if (out < sizeof(ULONG) || !irp->UserBuffer) { st = STATUS_BUFFER_TOO_SMALL; break; }
        for (ULONG i = 0; i < in; i++) sum += src[i];
        *(ULONG *)irp->UserBuffer = sum;
        info = sizeof sum;
        break;
    }
    case IOCTL_VEIL_STATS:
        if (out < 3 * sizeof(ULONG)) { st = STATUS_BUFFER_TOO_SMALL; break; }
        RtlCopyMemory(buf, &x->Creates, 3 * sizeof(ULONG));
        info = 3 * sizeof(ULONG);
        break;
    default:
        st = STATUS_INVALID_DEVICE_REQUEST;
    }
    DbgPrint("veiltest: ioctl %08lx in %lu out %lu -> %08lx (%Iu bytes)\n", code, in, out, st, info);
    irp->IoStatus.Status = st;
    irp->IoStatus.Information = info;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return st;
}

static VOID VeilThread(PVOID ctx) {
    UNREFERENCED_PARAMETER(ctx);
    KeWaitForSingleObject(&g_go, Executive, KernelMode, FALSE, NULL);
    g_thread_value = 7;
    PsTerminateSystemThread(STATUS_SUCCESS);
}

static VOID VeilDpc(PKDPC dpc, PVOID ctx, PVOID a1, PVOID a2) {
    UNREFERENCED_PARAMETER(dpc); UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a1); UNREFERENCED_PARAMETER(a2);
    g_dpc_irql = KeGetCurrentIrql();
}

static VOID VeilTimerDpc(PKDPC dpc, PVOID ctx, PVOID a1, PVOID a2) {
    UNREFERENCED_PARAMETER(dpc); UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a1); UNREFERENCED_PARAMETER(a2);
    if (++g_ticks == 3) KeSetEvent(&g_done, IO_NO_INCREMENT, FALSE);
}

static VOID VeilWork(PDEVICE_OBJECT dev, PVOID ctx) {
    UNREFERENCED_PARAMETER(dev);
    DbgPrint("veiltest: work item at irql %d, context %p\n", KeGetCurrentIrql(), ctx);
    KeSetEvent(&g_done, IO_NO_INCREMENT, FALSE);
}

// -----| structured exception handling |-----

// An address nothing maps: the far end of VEILOS's driver image region
// (on Windows, any unmapped kernel address faults the same way).
static volatile ULONG_PTR g_bad = 0xFFFFE07F00000000ULL;
static volatile LONG g_finally_runs;

static ULONG SehFault(void) {
    ULONG code = 0;
    __try {
        *(volatile ULONG *)g_bad = 1;
    } __except (code = GetExceptionCode(), EXCEPTION_EXECUTE_HANDLER) {
        DbgPrint("veiltest: page fault caught, code %08lx\n", code);
    }
    return code;
}

// Raised two calls down, through a __finally in each that must run on the
// way out.
static __declspec(noinline) void Inner(void) {
    __try {
        ExRaiseStatus(STATUS_INSUFFICIENT_RESOURCES);
    } __finally {
        g_finally_runs++;
    }
}

static __declspec(noinline) void Middle(void) {
    __try {
        Inner();
    } __finally {
        g_finally_runs++;
    }
}

static NTSTATUS SehRaise(void) {
    NTSTATUS st = STATUS_SUCCESS;
    __try {
        Middle();
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        st = GetExceptionCode();
    }
    return st;
}

static BOOLEAN SehProbe(void) {
    BOOLEAN bad_caught = FALSE, good_passed = FALSE;
    __try {
        ProbeForRead((PVOID)0xFFFF800000001000ULL, 16, 1); // a kernel address: refused
    } __except (GetExceptionCode() == STATUS_ACCESS_VIOLATION ? EXCEPTION_EXECUTE_HANDLER : EXCEPTION_CONTINUE_SEARCH) {
        bad_caught = TRUE;
    }
    __try {
        ProbeForRead((PVOID)0x10000, 16, 4); // a user address: passes
        good_passed = TRUE;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
    }
    return bad_caught && good_passed;
}

// A continuable exception whose filter says to continue: execution goes on
// after RtlRaiseException.
static LONG ContinueFilter(PEXCEPTION_POINTERS p) {
    return p->ExceptionRecord->ExceptionCode == 0x20001234 ? EXCEPTION_CONTINUE_EXECUTION : EXCEPTION_CONTINUE_SEARCH;
}

static BOOLEAN SehContinue(void) {
    volatile BOOLEAN after = FALSE;
    EXCEPTION_RECORD r = { 0 };
    r.ExceptionCode = 0x20001234;
    __try {
        RtlRaiseException(&r);
        after = TRUE;
    } __except (ContinueFilter(GetExceptionInformation())) {
    }
    return after;
}

static void SelfTest(PUNICODE_STRING reg, PDEVICE_OBJECT dev) {
    LARGE_INTEGER second, due;
    NTSTATUS st;
    second.QuadPart = -10000000LL;

    // Registry: a Parameters key under our Services key, a value set and read back.
    {
        OBJECT_ATTRIBUTES oa;
        UNICODE_STRING sub, val;
        HANDLE svc, params;
        ULONG answer = 42, disp = 0, got = 0;
        UCHAR info[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + 8];
        InitializeObjectAttributes(&oa, reg, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, NULL, NULL);
        st = ZwOpenKey(&svc, KEY_ALL_ACCESS, &oa);
        if (NT_SUCCESS(st)) {
            RtlInitUnicodeString(&sub, L"Parameters");
            InitializeObjectAttributes(&oa, &sub, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, svc, NULL);
            st = ZwCreateKey(&params, KEY_ALL_ACCESS, &oa, 0, NULL, 0, &disp);
            if (NT_SUCCESS(st)) {
                RtlInitUnicodeString(&val, L"Answer");
                ZwSetValueKey(params, &val, 0, REG_DWORD, &answer, sizeof answer);
                st = ZwQueryValueKey(params, &val, KeyValuePartialInformation, info, sizeof info, &got);
                if (NT_SUCCESS(st) && ((PKEY_VALUE_PARTIAL_INFORMATION)info)->Type == REG_DWORD &&
                    *(ULONG *)((PKEY_VALUE_PARTIAL_INFORMATION)info)->Data == 42)
                    g_passed |= T_REGISTRY;
                DbgPrint("veiltest: registry key %s, value read back %08lx\n", disp == REG_CREATED_NEW_KEY ? "created" : "opened", st);
                ZwClose(params);
            }
            ZwClose(svc);
        }
        RtlWriteRegistryValue(RTL_REGISTRY_SERVICES, L"veiltest\\Parameters", L"Greeting", REG_SZ, L"hello", 12);
    }

    // A system thread: waits for g_go, sets a value, ends; we wait on its object.
    {
        HANDLE th;
        PVOID obj;
        KeInitializeEvent(&g_go, SynchronizationEvent, FALSE);
        st = PsCreateSystemThread(&th, THREAD_ALL_ACCESS, NULL, NULL, NULL, VeilThread, NULL);
        if (NT_SUCCESS(st) && NT_SUCCESS(ObReferenceObjectByHandle(th, SYNCHRONIZE, NULL, KernelMode, &obj, NULL))) {
            KeSetEvent(&g_go, IO_NO_INCREMENT, FALSE);
            st = KeWaitForSingleObject(obj, Executive, KernelMode, FALSE, &second);
            if (st == STATUS_SUCCESS && g_thread_value == 7) g_passed |= T_THREAD;
            DbgPrint("veiltest: system thread wait %08lx, value %ld\n", st, g_thread_value);
            ObDereferenceObject(obj);
            ZwClose(th);
        }
    }

    // A DPC queued at PASSIVE_LEVEL runs at once, at DISPATCH_LEVEL.
    KeInitializeDpc(&g_dpc, VeilDpc, NULL);
    KeInsertQueueDpc(&g_dpc, NULL, NULL);
    KeFlushQueuedDpcs();
    if (g_dpc_irql == DISPATCH_LEVEL) g_passed |= T_DPC;
    DbgPrint("veiltest: DPC ran at irql %d\n", g_dpc_irql);

    // A periodic timer (10 ms, then every 5 ms) whose DPC sets g_done on its third run.
    KeInitializeEvent(&g_done, NotificationEvent, FALSE);
    KeInitializeDpc(&g_timer_dpc, VeilTimerDpc, NULL);
    KeInitializeTimer(&g_timer);
    due.QuadPart = -100000LL;
    KeSetTimerEx(&g_timer, due, 5, &g_timer_dpc);
    st = KeWaitForSingleObject(&g_done, Executive, KernelMode, FALSE, &second);
    KeCancelTimer(&g_timer);
    if (st == STATUS_SUCCESS && g_ticks >= 3) g_passed |= T_TIMER;
    DbgPrint("veiltest: timer wait %08lx after %ld DPC runs\n", st, g_ticks);

    // A work item, run on a thread of its own at PASSIVE_LEVEL.
    {
        PIO_WORKITEM w = IoAllocateWorkItem(dev);
        KeClearEvent(&g_done);
        if (w) {
            IoQueueWorkItem(w, VeilWork, DelayedWorkQueue, (PVOID)(ULONG_PTR)0x1234);
            st = KeWaitForSingleObject(&g_done, Executive, KernelMode, FALSE, &second);
            if (st == STATUS_SUCCESS) g_passed |= T_WORKITEM;
            DbgPrint("veiltest: work item wait %08lx\n", st);
            IoFreeWorkItem(w);
        }
    }
    // Structured exception handling.
    {
        ULONG fault = SehFault();
        NTSTATUS raised;
        if (fault == STATUS_ACCESS_VIOLATION) g_passed |= T_SEH_FAULT;
        g_finally_runs = 0;
        raised = SehRaise();
        if (raised == STATUS_INSUFFICIENT_RESOURCES) g_passed |= T_SEH_RAISE;
        if (g_finally_runs == 2) g_passed |= T_SEH_FINALLY;
        DbgPrint("veiltest: ExRaiseStatus caught %08lx, %ld __finally blocks ran\n", raised, g_finally_runs);
        if (SehProbe()) g_passed |= T_SEH_PROBE;
        if (SehContinue()) g_passed |= T_SEH_CONTINUE;
        DbgPrint("veiltest: ProbeForRead %s, continue-execution %s\n", (g_passed & T_SEH_PROBE) ? "ok" : "FAILED",
                 (g_passed & T_SEH_CONTINUE) ? "ok" : "FAILED");
    }
    DbgPrint("veiltest: self-test %lx of %x\n", g_passed, T_ALL);
}

// -----| waiting inside a program's request |-----
//
// Everything here runs in the IOCTL a program sent (DeviceIoControl), and
// each step waits for something another thread or the clock must do.
// Result bits: 1 a system thread's object, 2 a timer object, 4
// KeDelayExecutionThread, 8 a work item, 0x10 a timer's DPC.

static KEVENT g_wt_event;
static KTIMER g_wt_timer;
static KDPC g_wt_dpc;
static volatile LONG g_wt_dpc_runs;

static VOID WtThread(PVOID ctx) {
    UNREFERENCED_PARAMETER(ctx);
    g_thread_value = 9;
    PsTerminateSystemThread(STATUS_SUCCESS);
}

static VOID WtWork(PDEVICE_OBJECT dev, PVOID ctx) {
    UNREFERENCED_PARAMETER(dev); UNREFERENCED_PARAMETER(ctx);
    KeSetEvent(&g_wt_event, IO_NO_INCREMENT, FALSE);
}

static VOID WtDpc(PKDPC dpc, PVOID ctx, PVOID a1, PVOID a2) {
    UNREFERENCED_PARAMETER(dpc); UNREFERENCED_PARAMETER(ctx); UNREFERENCED_PARAMETER(a1); UNREFERENCED_PARAMETER(a2);
    InterlockedIncrement(&g_wt_dpc_runs);
    KeSetEvent(&g_wt_event, IO_NO_INCREMENT, FALSE);
}

static ULONG WaitTest(PDEVICE_OBJECT dev) {
    ULONG mask = 0;
    LARGE_INTEGER two_s, due, before, after;
    NTSTATUS st;
    two_s.QuadPart = -20000000LL;

    {
        HANDLE th;
        PVOID obj;
        g_thread_value = 0;
        if (NT_SUCCESS(PsCreateSystemThread(&th, THREAD_ALL_ACCESS, NULL, NULL, NULL, WtThread, NULL)) &&
            NT_SUCCESS(ObReferenceObjectByHandle(th, SYNCHRONIZE, NULL, KernelMode, &obj, NULL))) {
            st = KeWaitForSingleObject(obj, Executive, KernelMode, FALSE, &two_s);
            if (st == STATUS_SUCCESS && g_thread_value == 9) mask |= 1;
            ObDereferenceObject(obj);
            ZwClose(th);
        }
    }

    KeInitializeTimer(&g_wt_timer);
    due.QuadPart = -200000LL; // 20 ms
    KeSetTimer(&g_wt_timer, due, NULL);
    st = KeWaitForSingleObject(&g_wt_timer, Executive, KernelMode, FALSE, &two_s);
    if (st == STATUS_SUCCESS) mask |= 2;

    KeQuerySystemTimePrecise(&before);
    due.QuadPart = -300000LL; // 30 ms
    st = KeDelayExecutionThread(KernelMode, FALSE, &due);
    KeQuerySystemTimePrecise(&after);
    DbgPrint("veiltest: 30 ms delay took %I64d (100 ns), %08lx\n", after.QuadPart - before.QuadPart, st);
    if (after.QuadPart - before.QuadPart >= 200000LL) mask |= 4;

    KeInitializeEvent(&g_wt_event, SynchronizationEvent, FALSE);
    {
        PIO_WORKITEM w = IoAllocateWorkItem(dev);
        if (w) {
            IoQueueWorkItem(w, WtWork, DelayedWorkQueue, NULL);
            if (KeWaitForSingleObject(&g_wt_event, Executive, KernelMode, FALSE, &two_s) == STATUS_SUCCESS) mask |= 8;
            IoFreeWorkItem(w);
        }
    }

    g_wt_dpc_runs = 0;
    KeInitializeDpc(&g_wt_dpc, WtDpc, NULL);
    KeInitializeTimer(&g_wt_timer);
    due.QuadPart = -100000LL; // 10 ms
    KeSetTimer(&g_wt_timer, due, &g_wt_dpc);
    if (KeWaitForSingleObject(&g_wt_event, Executive, KernelMode, FALSE, &two_s) == STATUS_SUCCESS && g_wt_dpc_runs == 1)
        mask |= 0x10;

    DbgPrint("veiltest: waits inside a request: %lx of 1f\n", mask);
    return mask;
}

void VeilUnload(PDRIVER_OBJECT drv) {
    IoDeleteSymbolicLink(&g_link);
    if (drv->DeviceObject) IoDeleteDevice(drv->DeviceObject);
    if (g_scratch) ExFreePoolWithTag(g_scratch, 'liev');
    DbgPrint("veiltest: unloaded\n");
}

NTSTATUS DriverEntry(PDRIVER_OBJECT drv, PUNICODE_STRING reg) {
    UNICODE_STRING name;
    PDEVICE_OBJECT dev;
    NTSTATUS st;
    RTL_OSVERSIONINFOW ver = { sizeof ver };
    KEVENT ev;
    LARGE_INTEGER now, zero = { 0 };
    KSPIN_LOCK lock;
    KIRQL old;

    DbgPrint("veiltest: DriverEntry, registry path %wZ, irql %d\n", reg, KeGetCurrentIrql());

    g_scratch = ExAllocatePool2(POOL_FLAG_NON_PAGED, 8192, 'liev');
    if (!g_scratch) return STATUS_INSUFFICIENT_RESOURCES;
    DbgPrint("veiltest: pool %p, first word %Ix (zeroed: %s)\n", g_scratch, *(ULONG_PTR *)g_scratch,
             *(ULONG_PTR *)g_scratch == 0 ? "yes" : "NO");

    RtlGetVersion(&ver);
    KeQuerySystemTimePrecise(&now);
    DbgPrint("veiltest: Windows %lu.%lu build %lu; system time %I64d\n", ver.dwMajorVersion, ver.dwMinorVersion,
             ver.dwBuildNumber, now.QuadPart);

    KeInitializeSpinLock(&lock);
    KeAcquireSpinLock(&lock, &old);
    DbgPrint("veiltest: spinlock held at irql %d (was %d)\n", KeGetCurrentIrql(), old);
    KeReleaseSpinLock(&lock, old);

    KeInitializeEvent(&ev, NotificationEvent, FALSE);
    st = KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, &zero);
    DbgPrint("veiltest: unsignalled event wait -> %08lx (timeout is 102)\n", st);
    KeSetEvent(&ev, IO_NO_INCREMENT, FALSE);
    st = KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, &zero);
    DbgPrint("veiltest: signalled event wait -> %08lx\n", st);

    RtlInitUnicodeString(&name, L"\\Device\\VeilTest");
    st = IoCreateDevice(drv, sizeof(VEIL_EXT), &name, FILE_DEVICE_UNKNOWN, FILE_DEVICE_SECURE_OPEN, FALSE, &dev);
    if (!NT_SUCCESS(st)) {
        ExFreePoolWithTag(g_scratch, 'liev');
        return st;
    }
    KeInitializeSpinLock(&((VEIL_EXT *)dev->DeviceExtension)->Lock);
    RtlInitUnicodeString(&g_link, L"\\DosDevices\\VeilTest");
    st = IoCreateSymbolicLink(&g_link, &name);
    if (!NT_SUCCESS(st)) {
        IoDeleteDevice(dev);
        ExFreePoolWithTag(g_scratch, 'liev');
        return st;
    }
    drv->MajorFunction[IRP_MJ_CREATE] = VeilCreateClose;
    drv->MajorFunction[IRP_MJ_CLOSE] = VeilCreateClose;
    drv->MajorFunction[IRP_MJ_DEVICE_CONTROL] = VeilIoctl;
    drv->DriverUnload = VeilUnload;
    dev->Flags &= ~DO_DEVICE_INITIALIZING;
    SelfTest(reg, dev);
    DbgPrint("veiltest: \\Device\\VeilTest ready (stack size %d)\n", dev->StackSize);
    return STATUS_SUCCESS;
}
