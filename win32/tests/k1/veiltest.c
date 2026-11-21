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
int __cdecl sprintf_s(char *buffer, size_t size, const char *format, ...);
int __cdecl swprintf_s(wchar_t *buffer, size_t size, const wchar_t *format, ...);

#define IOCTL_VEIL_ECHO  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_ADD   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_STATS CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_SELFTEST CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_NEITHER  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x804, METHOD_NEITHER, FILE_ANY_ACCESS)
// Waits inside the request itself (a program's system call): see WaitTest.
#define IOCTL_VEIL_WAITTEST CTL_CODE(FILE_DEVICE_UNKNOWN, 0x805, METHOD_BUFFERED, FILE_ANY_ACCESS)
// The output buffer as an MDL of the program's pages: filled with a pattern
// seeded by the first input byte.
#define IOCTL_VEIL_DIRECT   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x806, METHOD_OUT_DIRECT, FILE_ANY_ACCESS)

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
#define T_CONTIGUOUS   0x400
#define T_IOSPACE      0x800
#define T_MDL_POOL     0x1000
#define T_MDL_PAGES    0x2000
#define T_PRINTF       0x4000
#define T_STRINGS      0x8000
#define T_BITMAP       0x10000
#define T_GENTABLE     0x20000
#define T_LOCKS        0x40000
#define T_LOOKASIDE    0x80000
#define T_TIME_REG     0x100000
#define T_EXTIMER_CB   0x200000
#define T_KPCR         0x400000
#define T_ALL      0x7FFFFF

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
    case IOCTL_VEIL_DIRECT: {
        UCHAR *dst = irp->MdlAddress ? (UCHAR *)MmGetSystemAddressForMdlSafe(irp->MdlAddress, NormalPagePriority) : NULL;
        UCHAR seed = in ? buf[0] : 0;
        if (!dst) { st = STATUS_INSUFFICIENT_RESOURCES; break; }
        for (ULONG i = 0; i < out; i++) dst[i] = (UCHAR)(seed + i * 7);
        info = out;
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

static PKTHREAD g_sys_kthread;

static VOID VeilThread(PVOID ctx) {
    UNREFERENCED_PARAMETER(ctx);
    g_sys_kthread = KeGetCurrentThread(); // gs:[0x188], inline
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

// -----| the run-time library and executive support |-----

static RTL_GENERIC_COMPARE_RESULTS NTAPI GtCompare(PRTL_GENERIC_TABLE t, PVOID a, PVOID b) {
    ULONG x = *(ULONG *)a, y = *(ULONG *)b;
    UNREFERENCED_PARAMETER(t);
    return x < y ? GenericLessThan : x > y ? GenericGreaterThan : GenericEqual;
}
static PVOID NTAPI GtAlloc(PRTL_GENERIC_TABLE t, CLONG size) {
    UNREFERENCED_PARAMETER(t);
    return ExAllocatePool2(POOL_FLAG_NON_PAGED, size, 'liev');
}
static VOID NTAPI GtFree(PRTL_GENERIC_TABLE t, PVOID p) {
    UNREFERENCED_PARAMETER(t);
    ExFreePoolWithTag(p, 'liev');
}

static KEVENT g_rt_event;
static volatile LONG g_cb_value;

static VOID ExTimerCallback(PEX_TIMER timer, PVOID ctx) {
    UNREFERENCED_PARAMETER(timer);
    if (ctx == (PVOID)0x77) KeSetEvent(&g_rt_event, IO_NO_INCREMENT, FALSE);
}

static VOID NamedCallback(PVOID ctx, PVOID a1, PVOID a2) {
    g_cb_value = (LONG)(ULONG_PTR)ctx + (LONG)(ULONG_PTR)a1 + (LONG)(ULONG_PTR)a2;
}

static ULONG RuntimeTest(void) {
    ULONG mask = 0;

    // printf, narrow and wide.
    {
        char nb[64];
        WCHAR wb[64];
        int n = sprintf_s(nb, sizeof nb, "%s-%d-%04x-%ls|%-4s|%+d", "a", -5, 0xab, L"w", "ab", 7);
        int m = swprintf_s(wb, 64, L"%s|%S|%5.2d|%c", L"wide", "narrow", 7, L'z');
        if (n == 19 && strcmp(nb, "a--5-00ab-w|ab  |+7") == 0 && m == 19 && wcscmp(wb, L"wide|narrow|   07|z") == 0)
            mask |= T_PRINTF;
        DbgPrint("veiltest: printf \"%s\" (%d)\n", nb, n);
    }
    // Counted strings and numbers.
    {
        ANSI_STRING a;
        UNICODE_STRING u, hex;
        WCHAR room[32];
        ULONG v = 0, w = 0;
        UNICODE_STRING dst = { 0, sizeof room, room };
        RtlInitAnsiString(&a, "Veil");
        if (NT_SUCCESS(RtlAnsiStringToUnicodeString(&u, &a, TRUE))) {
            RtlAppendUnicodeStringToString(&dst, &u);
            RtlAppendUnicodeToString(&dst, L"OS");
            RtlFreeUnicodeString(&u);
        }
        RtlInitUnicodeString(&hex, L"0x1F");
        RtlUnicodeStringToInteger(&hex, 0, &v);
        RtlCharToInteger("-12", 10, &w);
        if (dst.Length == 12 && wcsncmp(room, L"VeilOS", 6) == 0 && v == 31 && (LONG)w == -12) mask |= T_STRINGS;
    }
    // Bitmaps.
    {
        ULONG words[4] = { 0 };
        RTL_BITMAP bm;
        RtlInitializeBitMap(&bm, words, 100);
        RtlSetBits(&bm, 10, 10);
        if (RtlFindClearBits(&bm, 5, 8) == 20 && RtlNumberOfSetBits(&bm) == 10 && RtlTestBit(&bm, 15) &&
            !RtlTestBit(&bm, 20) && RtlAreBitsSet(&bm, 10, 10) && RtlAreBitsClear(&bm, 20, 80) && words[0] == 0x000FFC00)
            mask |= T_BITMAP;
    }
    // Generic tables: kept in order, found, deleted.
    {
        RTL_GENERIC_TABLE t;
        ULONG k, *e1, *e2, *e3;
        PVOID restart = NULL;
        BOOLEAN isnew;
        RtlInitializeGenericTable(&t, GtCompare, GtAlloc, GtFree, NULL);
        k = 5; RtlInsertElementGenericTable(&t, &k, sizeof k, &isnew);
        k = 1; RtlInsertElementGenericTable(&t, &k, sizeof k, &isnew);
        k = 3; RtlInsertElementGenericTable(&t, &k, sizeof k, &isnew);
        k = 3; RtlInsertElementGenericTable(&t, &k, sizeof k, &isnew); // already there
        e1 = (ULONG *)RtlEnumerateGenericTableWithoutSplaying(&t, &restart);
        e2 = (ULONG *)RtlEnumerateGenericTableWithoutSplaying(&t, &restart);
        e3 = (ULONG *)RtlEnumerateGenericTableWithoutSplaying(&t, &restart);
        k = 3;
        if (!isnew && e1 && e2 && e3 && *e1 == 1 && *e2 == 3 && *e3 == 5 && RtlLookupElementGenericTable(&t, &k) == e2 &&
            RtlDeleteElementGenericTable(&t, &k) && RtlNumberGenericTableElements(&t) == 2 && !RtlLookupElementGenericTable(&t, &k))
            mask |= T_GENTABLE;
        k = 1; RtlDeleteElementGenericTable(&t, &k);
        k = 5; RtlDeleteElementGenericTable(&t, &k);
    }
    // Resources (recursive exclusive, shared inside it), fast and guarded mutexes.
    {
        ERESOURCE *r = (ERESOURCE *)ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof(ERESOURCE), 'liev');
        FAST_MUTEX fm;
        KGUARDED_MUTEX gm;
        BOOLEAN ok = FALSE;
        KIRQL inside;
        if (r) {
            ExInitializeResourceLite(r);
            ok = ExAcquireResourceExclusiveLite(r, TRUE) && ExAcquireResourceSharedLite(r, TRUE) &&
                 ExIsResourceAcquiredExclusiveLite(r);
            ExReleaseResourceLite(r);
            ExReleaseResourceLite(r);
            ok = ok && !ExIsResourceAcquiredExclusiveLite(r) && ExAcquireResourceSharedLite(r, TRUE) &&
                 ExIsResourceAcquiredSharedLite(r) > 0;
            ExReleaseResourceLite(r);
            ExDeleteResourceLite(r);
            ExFreePoolWithTag(r, 'liev');
        }
        ExInitializeFastMutex(&fm);
        ExAcquireFastMutex(&fm);
        inside = KeGetCurrentIrql();
        ok = ok && !ExTryToAcquireFastMutex(&fm);
        ExReleaseFastMutex(&fm);
        ok = ok && inside == APC_LEVEL && KeGetCurrentIrql() == PASSIVE_LEVEL && ExTryToAcquireFastMutex(&fm);
        ExReleaseFastMutex(&fm);
        KeInitializeGuardedMutex(&gm);
        KeAcquireGuardedMutex(&gm);
        KeReleaseGuardedMutex(&gm);
        if (ok) mask |= T_LOCKS;
    }
    // Lookaside lists (a freed block comes back) and SLists.
    {
        NPAGED_LOOKASIDE_LIST *l = (NPAGED_LOOKASIDE_LIST *)ExAllocatePool2(POOL_FLAG_NON_PAGED, sizeof *l, 'liev');
        SLIST_HEADER head;
        SLIST_ENTRY *items = (SLIST_ENTRY *)ExAllocatePool2(POOL_FLAG_NON_PAGED, 3 * sizeof(SLIST_ENTRY), 'liev');
        BOOLEAN ok = FALSE;
        if (l && items) {
            PVOID p, q;
            ExInitializeNPagedLookasideList(l, NULL, NULL, 0, 64, 'liev', 0);
            p = ExAllocateFromNPagedLookasideList(l);
            ExFreeToNPagedLookasideList(l, p);
            q = ExAllocateFromNPagedLookasideList(l);
            ok = p && p == q;
            ExFreeToNPagedLookasideList(l, q);
            ExDeleteNPagedLookasideList(l);

            InitializeSListHead(&head);
            for (int i = 0; i < 3; i++) InterlockedPushEntrySList(&head, &items[i]);
            ok = ok && ExQueryDepthSList(&head) == 3 && InterlockedPopEntrySList(&head) == &items[2] &&
                 ExQueryDepthSList(&head) == 2;
        }
        if (l) ExFreePoolWithTag(l, 'liev');
        if (items) ExFreePoolWithTag(items, 'liev');
        if (ok) mask |= T_LOOKASIDE;
    }
    // Time fields, and RtlQueryRegistryValues (the Answer value DriverEntry
    // stored, and a default for one that isn't there).
    {
        LARGE_INTEGER epoch;
        TIME_FIELDS tf;
        ULONG answer = 0, missing = 0, dflt = 99;
        RTL_QUERY_REGISTRY_TABLE q[3];
        NTSTATUS st;
        epoch.QuadPart = 116444736000000000LL; // 1970-01-01, a Thursday
        RtlTimeToTimeFields(&epoch, &tf);
        RtlZeroMemory(q, sizeof q);
        q[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
        q[0].Name = L"Answer";
        q[0].EntryContext = &answer;
        q[1].Flags = RTL_QUERY_REGISTRY_DIRECT;
        q[1].Name = L"NotThere";
        q[1].EntryContext = &missing;
        q[1].DefaultType = REG_DWORD;
        q[1].DefaultData = &dflt;
        q[1].DefaultLength = sizeof dflt;
        st = RtlQueryRegistryValues(RTL_REGISTRY_SERVICES, L"veiltest\\Parameters", q, NULL, NULL);
        if (tf.Year == 1970 && tf.Month == 1 && tf.Day == 1 && tf.Weekday == 4 && st == STATUS_SUCCESS && answer == 42 && missing == 99)
            mask |= T_TIME_REG;
        DbgPrint("veiltest: %d-%d-%d (weekday %d); registry query %08lx, Answer %lu\n", tf.Year, tf.Month, tf.Day, tf.Weekday, st, answer);
    }
    // An Ex timer, and a named callback object.
    {
        LARGE_INTEGER second;
        PEX_TIMER t = ExAllocateTimer(ExTimerCallback, (PVOID)0x77, 0);
        PCALLBACK_OBJECT cb = NULL;
        OBJECT_ATTRIBUTES oa;
        UNICODE_STRING name;
        BOOLEAN ok = FALSE;
        second.QuadPart = -10000000LL;
        KeInitializeEvent(&g_rt_event, NotificationEvent, FALSE);
        if (t) {
            ExSetTimer(t, -100000LL, 0, NULL); // 10 ms
            ok = KeWaitForSingleObject(&g_rt_event, Executive, KernelMode, FALSE, &second) == STATUS_SUCCESS;
            ExDeleteTimer(t, TRUE, FALSE, NULL);
        }
        RtlInitUnicodeString(&name, L"\\Callback\\VeilTest");
        InitializeObjectAttributes(&oa, &name, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, NULL, NULL);
        if (NT_SUCCESS(ExCreateCallback(&cb, &oa, TRUE, TRUE))) {
            PVOID reg = ExRegisterCallback(cb, NamedCallback, (PVOID)1);
            ExNotifyCallback(cb, (PVOID)10, (PVOID)100);
            ok = ok && g_cb_value == 111;
            ExUnregisterCallback(reg);
            ObDereferenceObject(cb);
        } else {
            ok = FALSE;
        }
        if (ok) mask |= T_EXTIMER_CB;
    }
    return mask;
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
    // Memory: contiguous, device (I/O space) mappings, MDLs.
    {
        PHYSICAL_ADDRESS low = { 0 }, high, boundary = { 0 }, pa0, pa1;
        UCHAR *c;
        high.QuadPart = 0xFFFFFFFFLL; // below 4 GiB, as DMA engines often need
        c = (UCHAR *)MmAllocateContiguousMemorySpecifyCache(3 * PAGE_SIZE, low, high, boundary, MmCached);
        if (c) {
            pa0 = MmGetPhysicalAddress(c);
            pa1 = MmGetPhysicalAddress(c + 2 * PAGE_SIZE + 5);
            if (pa0.QuadPart != 0 && pa0.QuadPart + 3 * PAGE_SIZE <= 0x100000000LL && pa1.QuadPart == pa0.QuadPart + 2 * PAGE_SIZE + 5 &&
                c[100] == 0)
                g_passed |= T_CONTIGUOUS;
            DbgPrint("veiltest: contiguous memory %p at physical %I64x\n", c, pa0.QuadPart);

            // The same memory again through MmMapIoSpace (cached, like the
            // first mapping: mixing cache types on one page isn't coherent on
            // real CPUs): writes through one are seen through the other.
            {
                UCHAR *io = (UCHAR *)MmMapIoSpace(pa0, 2 * PAGE_SIZE, MmCached);
                if (io && io != c) {
                    io[PAGE_SIZE + 3] = 0x5A;
                    c[17] = 0xA5;
                    if (c[PAGE_SIZE + 3] == 0x5A && io[17] == 0xA5) g_passed |= T_IOSPACE;
                    MmUnmapIoSpace(io, 2 * PAGE_SIZE);
                }
            }
            MmFreeContiguousMemory(c);
        }
    }
    {
        // An MDL over a pool buffer spanning pages: its PFNs agree with
        // MmGetPhysicalAddress.
        UCHAR *p = (UCHAR *)ExAllocatePool2(POOL_FLAG_NON_PAGED, 3 * PAGE_SIZE, 'liev');
        if (p) {
            PMDL m = IoAllocateMdl(p + 100, 2 * PAGE_SIZE, FALSE, FALSE, NULL);
            if (m) {
                PPFN_NUMBER pfn;
                MmBuildMdlForNonPagedPool(m);
                pfn = MmGetMdlPfnArray(m);
                if (pfn[0] == (PFN_NUMBER)(MmGetPhysicalAddress(p + 100).QuadPart >> PAGE_SHIFT) &&
                    pfn[2] == (PFN_NUMBER)(MmGetPhysicalAddress(p + 100 + 2 * PAGE_SIZE).QuadPart >> PAGE_SHIFT) &&
                    MmGetSystemAddressForMdlSafe(m, NormalPagePriority) == p + 100)
                    g_passed |= T_MDL_POOL;
                IoFreeMdl(m);
            }
            ExFreePoolWithTag(p, 'liev');
        }
    }
    {
        // Pages for an MDL, mapped into system space, written and read back.
        PHYSICAL_ADDRESS low = { 0 }, high, skip = { 0 };
        PMDL m;
        high.QuadPart = -1LL;
        m = MmAllocatePagesForMdlEx(low, high, skip, 3 * PAGE_SIZE, MmCached, 0);
        if (m) {
            ULONG *v = (ULONG *)MmMapLockedPagesSpecifyCache(m, KernelMode, MmCached, NULL, FALSE, NormalPagePriority);
            if (v && MmGetMdlByteCount(m) == 3 * PAGE_SIZE) {
                ULONG ok = 1;
                for (ULONG i = 0; i < 3 * PAGE_SIZE / 4; i += 997) v[i] = i ^ 0xC0FFEE;
                for (ULONG i = 0; i < 3 * PAGE_SIZE / 4; i += 997) ok &= v[i] == (i ^ 0xC0FFEE);
                if (ok) g_passed |= T_MDL_PAGES;
                MmUnmapLockedPages(v, m);
            }
            MmFreePagesFromMdl(m);
            ExFreePool(m);
        }
    }
    DbgPrint("veiltest: memory: contiguous %d, I/O space %d, pool MDL %d, MDL pages %d\n", !!(g_passed & T_CONTIGUOUS),
             !!(g_passed & T_IOSPACE), !!(g_passed & T_MDL_POOL), !!(g_passed & T_MDL_PAGES));

    g_passed |= RuntimeTest();

    // The KPCR fields the WDK reads through GS without calling in.
    {
        PKTHREAD me = KeGetCurrentThread();
        PROCESSOR_NUMBER pn;
        ULONG n = KeGetCurrentProcessorNumberEx(&pn);
        PKPCR pcr = KeGetPcr();
        if (me && me == (PKTHREAD)PsGetCurrentThread() && g_sys_kthread && g_sys_kthread != me &&
            KeGetCurrentProcessorIndex() == n && pn.Number == n && pcr && pcr->Self == pcr)
            g_passed |= T_KPCR;
        DbgPrint("veiltest: thread %p (system thread %p), CPU %lu / %lu, PCR %p\n", me, g_sys_kthread, n,
                 KeGetCurrentProcessorIndex(), pcr);
    }
    DbgPrint("veiltest: run-time library and executive: %lx of %x\n", g_passed & 0x3FC000, 0x3FC000);

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

    // GS inside a program's request (after the system call's swapgs).
    if (KeGetCurrentThread() && KeGetCurrentThread() != g_sys_kthread && KeGetPcr()->Self == KeGetPcr()) mask |= 0x20;
    DbgPrint("veiltest: waits inside a request: %lx of 3f\n", mask);
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
