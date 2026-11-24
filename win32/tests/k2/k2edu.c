// K2: a WDM PnP function driver for QEMU's "edu" teaching device
// (PCI 1234:11E8), for VEILOS's driver host stage W2 (WDDM_DESIGN.md):
// AddDevice, START_DEVICE with resources from PCI, a message-signaled
// interrupt through IoConnectInterruptEx, ISR -> DPC, power and PnP calls.
// Every edu device gets its own stack: the test VM has two, and k2test.exe
// checks they stay apart (resources, vectors, interrupts).
//
// edu's registers (BAR0): 0x00 identification (0x010000ed), 0x04 liveness
// (reads back inverted), 0x08 factorial, 0x20 status (bit 0 computing, bit
// 7 interrupt when the factorial is done), 0x24 interrupt status, 0x60
// raise interrupt (ORs the value into the status), 0x64 acknowledge.
//
// Built with MSVC against the WDK, like K1 (build.sh). Results are a bit
// mask per device, read with IOCTL_K2_RESULT; k2test.exe checks them.
#include <ntifs.h>
#include <initguid.h>
#include <wdmguid.h>
#include <ntstrsafe.h>

#define IOCTL_K2_RESULT CTL_CODE(FILE_DEVICE_UNKNOWN, 0x900, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_K2_RAISE  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x901, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_K2_PING   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x902, METHOD_BUFFERED, FILE_ANY_ACCESS)

#define T_ATTACH      0x000001
#define T_PROPERTY    0x000002
#define T_REGKEY      0x000004
#define T_INTERFACE   0x000008
#define T_RESOURCES   0x000010
#define T_MMIO        0x000020
#define T_BUSIF       0x000040
#define T_CONNECT     0x000080
#define T_INTERRUPTS  0x000100
#define T_LATENCY     0x000200
#define T_FACTORIAL   0x000400
#define T_SYNC        0x000800
#define T_REMOVELOCK  0x001000
#define T_READCONFIG  0x002000
#define T_POWER       0x004000
#define T_TIMER       0x008000
#define T_CROSS       0x010000
#define T_CANCEL      0x020000
#define T_NOTIFY      0x040000
#define T_EVENTS      0x080000
#define T_MISC        0x100000
#define T_PROPDATA    0x200000
#define T_ALL         0x3FFFFF

// {6E3F0F6B-2C2A-4C61-9B8E-7E5A1B4C2D10}: the edu interface.
static const GUID GUID_VEIL_EDU = { 0x6e3f0f6b, 0x2c2a, 0x4c61, { 0x9b, 0x8e, 0x7e, 0x5a, 0x1b, 0x4c, 0x2d, 0x10 } };
// A property of our own: {6E3F0F6B-...-2D11}, 2.
static const DEVPROPKEY KEY_VEIL = { { 0x6e3f0f6b, 0x2c2a, 0x4c61, { 0x9b, 0x8e, 0x7e, 0x5a, 0x1b, 0x4c, 0x2d, 0x11 } }, 2 };

typedef struct {
    ULONG Mask, Index, Bus, Address, Vector, Irql;
    ULONG IsrCount, DpcCount, LatencyUs, TimerTicks, OtherIndex, Factorial;
} K2_RESULT;

typedef struct {
    PDEVICE_OBJECT Self, Lower, Pdo;
    IO_REMOVE_LOCK RemoveLock;
    ULONG Index;
    volatile ULONG *Regs;
    PIO_INTERRUPT_MESSAGE_INFO Messages;
    PKINTERRUPT Interrupt;
    ULONG ConnectVersion;
    ULONG Vector;
    KIRQL Irql;
    volatile LONG IsrCount, DpcCount, TimerTicks;
    volatile ULONG Status;
    LONGLONG IsrTime, LatencySum;
    ULONG LatencyN;
    KEVENT DpcEvent;
    UNICODE_STRING Interface;
    UNICODE_STRING Link;
    ULONG Mask;
    ULONG Factorial;
    volatile LONG PowerSeen, PowerDone;
    BOOLEAN Started, Connected, TimerOn;
} EXT;

static volatile LONG g_devices;
static volatile LONG g_notified;

static ULONG Reg(EXT *x, ULONG off) { return x->Regs[off / 4]; }
static void SetReg(EXT *x, ULONG off, ULONG v) { x->Regs[off / 4] = v; }

// -----| interrupt |-----

static BOOLEAN EduMessageIsr(PKINTERRUPT irq, PVOID ctx, ULONG id) {
    EXT *x = (EXT *)ctx;
    ULONG st;
    UNREFERENCED_PARAMETER(irq);
    UNREFERENCED_PARAMETER(id);
    st = Reg(x, 0x24);
    if (!st) return FALSE;
    SetReg(x, 0x64, st);
    x->Status |= st;
    InterlockedIncrement(&x->IsrCount);
    if (KeGetCurrentIrql() > DISPATCH_LEVEL) x->Irql = KeGetCurrentIrql();
    x->IsrTime = KeQueryPerformanceCounter(NULL).QuadPart;
    IoRequestDpc(x->Self, NULL, (PVOID)(ULONG_PTR)st);
    return TRUE;
}

static BOOLEAN EduLineIsr(PKINTERRUPT irq, PVOID ctx) {
    return EduMessageIsr(irq, ctx, 0);
}

static void EduDpc(PKDPC dpc, PDEVICE_OBJECT dev, PIRP irp, PVOID ctx) {
    EXT *x = (EXT *)dev->DeviceExtension;
    LARGE_INTEGER f;
    LONGLONG now = KeQueryPerformanceCounter(&f).QuadPart;
    UNREFERENCED_PARAMETER(dpc);
    UNREFERENCED_PARAMETER(irp);
    UNREFERENCED_PARAMETER(ctx);
    if (x->IsrTime && f.QuadPart) {
        x->LatencySum += (now - x->IsrTime) * 1000000 / f.QuadPart;
        x->LatencyN++;
    }
    InterlockedIncrement(&x->DpcCount);
    KeSetEvent(&x->DpcEvent, IO_NO_INCREMENT, FALSE);
}

// Raise the interrupt with `bit` and wait (1 s at most) for its DPC.
static BOOLEAN Raise(EXT *x, ULONG bit) {
    LARGE_INTEGER t;
    t.QuadPart = -10000000LL;
    KeClearEvent(&x->DpcEvent);
    x->Status = 0;
    SetReg(x, 0x60, bit);
    if (KeWaitForSingleObject(&x->DpcEvent, Executive, KernelMode, FALSE, &t) != STATUS_SUCCESS) return FALSE;
    return (x->Status & bit) != 0;
}

typedef struct { EXT *x; KIRQL seen; } SYNC_CTX;
static BOOLEAN SyncRoutine(PVOID c) {
    SYNC_CTX *s = (SYNC_CTX *)c;
    s->seen = KeGetCurrentIrql();
    return TRUE;
}

// -----| helpers |-----

static void IoTimerRoutine(PDEVICE_OBJECT dev, PVOID ctx) {
    EXT *x = (EXT *)dev->DeviceExtension;
    UNREFERENCED_PARAMETER(ctx);
    InterlockedIncrement(&x->TimerTicks);
}

static void PowerDone(PDEVICE_OBJECT dev, UCHAR minor, POWER_STATE state, PVOID ctx, PIO_STATUS_BLOCK iosb) {
    EXT *x = (EXT *)ctx;
    UNREFERENCED_PARAMETER(dev);
    UNREFERENCED_PARAMETER(minor);
    if (NT_SUCCESS(iosb->Status) && state.DeviceState == PowerDeviceD0) InterlockedExchange(&x->PowerDone, 1);
}

static void CancelRoutine(PDEVICE_OBJECT dev, PIRP irp) {
    UNREFERENCED_PARAMETER(dev);
    IoReleaseCancelSpinLock(irp->CancelIrql);
    irp->IoStatus.Status = STATUS_CANCELLED;
}

static NTSTATUS InterfaceArrived(PVOID n, PVOID ctx) {
    PDEVICE_INTERFACE_CHANGE_NOTIFICATION c = (PDEVICE_INTERFACE_CHANGE_NOTIFICATION)n;
    UNREFERENCED_PARAMETER(ctx);
    if (IsEqualGUID(&c->Event, &GUID_DEVICE_INTERFACE_ARRIVAL) && IsEqualGUID(&c->InterfaceClassGuid, &GUID_VEIL_EDU)) InterlockedIncrement(&g_notified);
    return STATUS_SUCCESS;
}

// A PnP request of our own down the stack (IoBuildSynchronousFsdRequest is
// how drivers make one).
static NTSTATUS ReadConfig(EXT *x, PVOID buf, ULONG off, ULONG len) {
    KEVENT ev;
    IO_STATUS_BLOCK iosb;
    PIRP irp;
    PIO_STACK_LOCATION sl;
    NTSTATUS st;
    KeInitializeEvent(&ev, NotificationEvent, FALSE);
    irp = IoBuildSynchronousFsdRequest(IRP_MJ_PNP, x->Lower, NULL, 0, NULL, &ev, &iosb);
    if (!irp) return STATUS_INSUFFICIENT_RESOURCES;
    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    sl = IoGetNextIrpStackLocation(irp);
    sl->MinorFunction = IRP_MN_READ_CONFIG;
    sl->Parameters.ReadWriteConfig.WhichSpace = PCI_WHICHSPACE_CONFIG;
    sl->Parameters.ReadWriteConfig.Buffer = buf;
    sl->Parameters.ReadWriteConfig.Offset = off;
    sl->Parameters.ReadWriteConfig.Length = len;
    st = IoCallDriver(x->Lower, irp);
    if (st == STATUS_PENDING) {
        KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, NULL);
        st = iosb.Status;
    }
    return NT_SUCCESS(st) && iosb.Information == len ? STATUS_SUCCESS : STATUS_UNSUCCESSFUL;
}

static NTSTATUS QueryBusInterface(EXT *x, BUS_INTERFACE_STANDARD *bus) {
    KEVENT ev;
    IO_STATUS_BLOCK iosb;
    PIRP irp;
    PIO_STACK_LOCATION sl;
    NTSTATUS st;
    KeInitializeEvent(&ev, NotificationEvent, FALSE);
    irp = IoBuildSynchronousFsdRequest(IRP_MJ_PNP, x->Lower, NULL, 0, NULL, &ev, &iosb);
    if (!irp) return STATUS_INSUFFICIENT_RESOURCES;
    irp->IoStatus.Status = STATUS_NOT_SUPPORTED;
    sl = IoGetNextIrpStackLocation(irp);
    sl->MinorFunction = IRP_MN_QUERY_INTERFACE;
    sl->Parameters.QueryInterface.InterfaceType = &GUID_BUS_INTERFACE_STANDARD;
    sl->Parameters.QueryInterface.Size = sizeof(*bus);
    sl->Parameters.QueryInterface.Version = 1;
    sl->Parameters.QueryInterface.Interface = (PINTERFACE)bus;
    sl->Parameters.QueryInterface.InterfaceSpecificData = NULL;
    st = IoCallDriver(x->Lower, irp);
    if (st == STATUS_PENDING) {
        KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, NULL);
        st = iosb.Status;
    }
    return st;
}

// -----| start: resources, interrupt, the tests that need the device |-----

static NTSTATUS Start(EXT *x, PCM_RESOURCE_LIST raw, PCM_RESOURCE_LIST trans) {
    PHYSICAL_ADDRESS mem = { 0 };
    ULONG memLen = 0, i;
    BOOLEAN haveMsg = FALSE;
    PCM_PARTIAL_RESOURCE_DESCRIPTOR intr = NULL;
    IO_CONNECT_INTERRUPT_PARAMETERS cp;
    NTSTATUS st;
    UNREFERENCED_PARAMETER(raw);

    if (!trans || trans->Count < 1) return STATUS_DEVICE_CONFIGURATION_ERROR;
    for (i = 0; i < trans->List[0].PartialResourceList.Count; i++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR d = &trans->List[0].PartialResourceList.PartialDescriptors[i];
        if (d->Type == CmResourceTypeMemory && !memLen) {
            mem = d->u.Memory.Start;
            memLen = d->u.Memory.Length;
        } else if (d->Type == CmResourceTypeInterrupt) {
            intr = d;
            haveMsg = (d->Flags & CM_RESOURCE_INTERRUPT_MESSAGE) != 0;
            x->Vector = d->u.MessageInterrupt.Translated.Vector;
        }
    }
    if (memLen >= PAGE_SIZE && intr && haveMsg) x->Mask |= T_RESOURCES;
    DbgPrint("k2edu %u: memory %llx (+%x), interrupt vector %x%s\n", x->Index, mem.QuadPart, memLen, x->Vector, haveMsg ? " (message)" : "");
    if (!memLen) return STATUS_DEVICE_CONFIGURATION_ERROR;

    x->Regs = (volatile ULONG *)MmMapIoSpaceEx(mem, PAGE_SIZE, PAGE_READWRITE | PAGE_NOCACHE);
    if (!x->Regs) return STATUS_INSUFFICIENT_RESOURCES;
    SetReg(x, 0x04, 0x12345678);
    if ((Reg(x, 0x00) & 0xFF) == 0xED && Reg(x, 0x04) == ~0x12345678u) x->Mask |= T_MMIO;
    DbgPrint("k2edu %u: identification %08x\n", x->Index, Reg(x, 0x00));

    {
        BUS_INTERFACE_STANDARD bus;
        USHORT ids[2] = { 0 };
        RtlZeroMemory(&bus, sizeof bus);
        if (NT_SUCCESS(QueryBusInterface(x, &bus)) && bus.GetBusData) {
            bus.GetBusData(bus.Context, PCI_WHICHSPACE_CONFIG, ids, 0, 4);
            if (bus.InterfaceDereference) bus.InterfaceDereference(bus.Context);
        }
        if (ids[0] == 0x1234 && ids[1] == 0x11E8) x->Mask |= T_BUSIF;
    }
    {
        USHORT ids[2] = { 0 };
        if (NT_SUCCESS(ReadConfig(x, ids, 0, 4)) && ids[0] == 0x1234 && ids[1] == 0x11E8) x->Mask |= T_READCONFIG;
    }

    // The interrupt, message-signaled.
    RtlZeroMemory(&cp, sizeof cp);
    cp.Version = CONNECT_MESSAGE_BASED;
    cp.MessageBased.PhysicalDeviceObject = x->Pdo;
    cp.MessageBased.ConnectionContext.InterruptMessageTable = &x->Messages;
    cp.MessageBased.MessageServiceRoutine = EduMessageIsr;
    cp.MessageBased.ServiceContext = x;
    cp.MessageBased.FallBackServiceRoutine = EduLineIsr;
    st = IoConnectInterruptEx(&cp);
    if (!NT_SUCCESS(st)) {
        DbgPrint("k2edu %u: IoConnectInterruptEx %08x\n", x->Index, st);
        return st;
    }
    x->ConnectVersion = cp.Version;
    x->Connected = TRUE;
    x->Interrupt = cp.Version == CONNECT_MESSAGE_BASED ? x->Messages->MessageInfo[0].InterruptObject : (PKINTERRUPT)x->Messages;
    if (cp.Version == CONNECT_MESSAGE_BASED && x->Messages->MessageCount == 1 && x->Messages->MessageInfo[0].Vector == x->Vector) x->Mask |= T_CONNECT;

    {
        ULONG ok = 0;
        for (i = 0; i < 8; i++) ok += Raise(x, 1u << i);
        if (ok == 8 && x->IsrCount >= 8 && x->DpcCount >= 8) x->Mask |= T_INTERRUPTS;
        if (x->LatencyN && x->LatencySum / x->LatencyN < 500) x->Mask |= T_LATENCY;
        DbgPrint("k2edu %u: %u of 8 interrupts, ISR at IRQL %u, DPC %llu us after its ISR on average\n",
                 x->Index, ok, x->Irql, x->LatencyN ? x->LatencySum / x->LatencyN : 0);
    }
    {
        // Factorial of 10, with an interrupt when it's done.
        LARGE_INTEGER t;
        t.QuadPart = -10000000LL;
        KeClearEvent(&x->DpcEvent);
        x->Status = 0;
        SetReg(x, 0x20, 0x80);
        SetReg(x, 0x08, 10);
        if (KeWaitForSingleObject(&x->DpcEvent, Executive, KernelMode, FALSE, &t) == STATUS_SUCCESS) x->Factorial = Reg(x, 0x08);
        SetReg(x, 0x20, 0);
        if (x->Factorial == 3628800) x->Mask |= T_FACTORIAL;
    }
    {
        SYNC_CTX s = { x, 0 };
        if (KeSynchronizeExecution(x->Interrupt, SyncRoutine, &s) && s.seen >= x->Irql && s.seen > DISPATCH_LEVEL) x->Mask |= T_SYNC;
    }
    {
        POWER_STATE ps;
        ps.DeviceState = PowerDeviceD0;
        if (PoRequestPowerIrp(x->Pdo, IRP_MN_SET_POWER, ps, PowerDone, x, NULL) == STATUS_PENDING && x->PowerDone && x->PowerSeen) x->Mask |= T_POWER;
    }
    if (NT_SUCCESS(IoInitializeTimer(x->Self, IoTimerRoutine, NULL))) {
        IoStartTimer(x->Self);
        x->TimerOn = TRUE;
    }
    if (x->Interface.Buffer && NT_SUCCESS(IoSetDeviceInterfaceState(&x->Interface, TRUE))) {
        PZZWSTR list = NULL;
        if (NT_SUCCESS(IoGetDeviceInterfaces(&GUID_VEIL_EDU, x->Pdo, 0, &list)) && list) {
            UNICODE_STRING first;
            RtlInitUnicodeString(&first, list);
            if (RtlEqualUnicodeString(&first, &x->Interface, TRUE)) x->Mask |= T_INTERFACE;
            ExFreePool(list);
        }
    }
    x->Started = TRUE;
    return STATUS_SUCCESS;
}

static void Stop(EXT *x) {
    if (x->TimerOn) IoStopTimer(x->Self);
    x->TimerOn = FALSE;
    if (x->Interface.Buffer) IoSetDeviceInterfaceState(&x->Interface, FALSE);
    if (x->Connected) {
        IO_DISCONNECT_INTERRUPT_PARAMETERS dp;
        dp.Version = x->ConnectVersion;
        dp.ConnectionContext.InterruptMessageTable = x->Messages;
        IoDisconnectInterruptEx(&dp);
        x->Connected = FALSE;
    }
    if (x->Regs) MmUnmapIoSpace((PVOID)x->Regs, PAGE_SIZE);
    x->Regs = NULL;
    x->Started = FALSE;
}

// -----| tests at request time (both devices exist by then) |-----

static ULONG Ping(EXT *x, ULONG other) {
    UNICODE_STRING name;
    WCHAR buf[32];
    PFILE_OBJECT file;
    PDEVICE_OBJECT dev;
    KEVENT ev;
    IO_STATUS_BLOCK iosb;
    ULONG reply = 0xFFFFFFFF;
    PIRP irp;
    NTSTATUS st;
    UNREFERENCED_PARAMETER(x);
    RtlInitEmptyUnicodeString(&name, buf, sizeof buf);
    RtlUnicodeStringPrintf(&name, L"\\Device\\VeilEdu%u", other);
    if (!NT_SUCCESS(IoGetDeviceObjectPointer(&name, FILE_READ_DATA, &file, &dev))) return reply;
    KeInitializeEvent(&ev, NotificationEvent, FALSE);
    irp = IoBuildDeviceIoControlRequest(IOCTL_K2_PING, dev, NULL, 0, &reply, sizeof reply, FALSE, &ev, &iosb);
    if (irp) {
        st = IoCallDriver(dev, irp);
        if (st == STATUS_PENDING) KeWaitForSingleObject(&ev, Executive, KernelMode, FALSE, NULL);
    }
    ObDereferenceObject(file);
    return reply;
}

static void RequestTests(EXT *x, K2_RESULT *r) {
    // The other edu device answers with its own index.
    r->OtherIndex = Ping(x, x->Index ^ 1);
    if (r->OtherIndex == (x->Index ^ 1)) x->Mask |= T_CROSS;
    if (x->TimerTicks >= 1) x->Mask |= T_TIMER;
    {
        PIRP irp = IoAllocateIrp(1, FALSE);
        if (irp) {
            irp->IoStatus.Status = STATUS_SUCCESS;
            IoSetCancelRoutine(irp, CancelRoutine);
            if (IoCancelIrp(irp) && irp->Cancel && irp->IoStatus.Status == STATUS_CANCELLED && !irp->CancelRoutine) x->Mask |= T_CANCEL;
            IoFreeIrp(irp);
        }
    }
    {
        PVOID entry = NULL;
        LONG before = g_notified;
        if (NT_SUCCESS(IoRegisterPlugPlayNotification(EventCategoryDeviceInterfaceChange, PNPNOTIFY_DEVICE_INTERFACE_INCLUDE_EXISTING_INTERFACES,
                (PVOID)&GUID_VEIL_EDU, x->Self->DriverObject, InterfaceArrived, NULL, &entry))) {
            if (g_notified - before >= g_devices) x->Mask |= T_NOTIFY;
            IoUnregisterPlugPlayNotificationEx(entry);
        }
    }
    {
        UNICODE_STRING name;
        HANDLE h = NULL, h2 = NULL;
        PKEVENT ev;
        LARGE_INTEGER zero = { 0 };
        PIO_ERROR_LOG_PACKET log = (PIO_ERROR_LOG_PACKET)IoAllocateErrorLogEntry(x->Self, (UCHAR)sizeof(IO_ERROR_LOG_PACKET));
        if (log) {
            log->ErrorCode = 0x40000001;
            log->UniqueErrorValue = x->Index;
            IoWriteErrorLogEntry(log);
        }
        RtlInitUnicodeString(&name, x->Index ? L"\\BaseNamedObjects\\VeilEdu1" : L"\\BaseNamedObjects\\VeilEdu0");
        ev = IoCreateNotificationEvent(&name, &h);
        if (ev && log && KeReadStateEvent(ev) &&
            NT_SUCCESS(ObOpenObjectByPointer(ev, OBJ_KERNEL_HANDLE, NULL, EVENT_ALL_ACCESS, *ExEventObjectType, KernelMode, &h2)) &&
            ZwWaitForSingleObject(h2, FALSE, &zero) == STATUS_SUCCESS) x->Mask |= T_EVENTS;
        if (h2) ZwClose(h2);
        if (h) ZwClose(h);
    }
    {
        ULONG_PTR lo = 0, hi = 0, here = (ULONG_PTR)&lo, n = 0;
        UNICODE_STRING path = { 0 };
        HANDLE k = NULL;
        BOOLEAN ok;
        PDEVICE_OBJECT devs[8];
        IoGetStackLimits(&lo, &hi);
        ok = lo < here && here < hi;
        ok = ok && NT_SUCCESS(IoQueryFullDriverPath(x->Self->DriverObject, &path)) && path.Length >= 18 &&
             _wcsnicmp(path.Buffer + path.Length / 2 - 9, L"k2edu.sys", 9) == 0;
        if (path.Buffer) ExFreePool(path.Buffer);
        ok = ok && NT_SUCCESS(IoOpenDriverRegistryKey(x->Self->DriverObject, DriverRegKeyParameters, KEY_READ, 0, &k));
        if (k) ZwClose(k);
        ok = ok && NT_SUCCESS(FsRtlRegisterFileSystemFilterCallbacks(x->Self->DriverObject, NULL));
        ok = ok && NT_SUCCESS(IoEnumerateDeviceObjectList(x->Self->DriverObject, devs, sizeof devs, (PULONG)&n)) && n == (ULONG_PTR)g_devices;
        if (ok) x->Mask |= T_MISC;
    }
    r->Mask = x->Mask;
    r->Index = x->Index;
    r->Vector = x->Vector;
    r->Irql = x->Irql;
    r->IsrCount = x->IsrCount;
    r->DpcCount = x->DpcCount;
    r->LatencyUs = x->LatencyN ? (ULONG)(x->LatencySum / x->LatencyN) : 0;
    r->TimerTicks = x->TimerTicks;
    r->Factorial = x->Factorial;
}

// -----| dispatch |-----

static NTSTATUS Complete(PIRP irp, NTSTATUS st, ULONG_PTR info) {
    irp->IoStatus.Status = st;
    irp->IoStatus.Information = info;
    IoCompleteRequest(irp, IO_NO_INCREMENT);
    return st;
}

static NTSTATUS DispatchCreateClose(PDEVICE_OBJECT dev, PIRP irp) {
    UNREFERENCED_PARAMETER(dev);
    return Complete(irp, STATUS_SUCCESS, 0);
}

static NTSTATUS DispatchControl(PDEVICE_OBJECT dev, PIRP irp) {
    EXT *x = (EXT *)dev->DeviceExtension;
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(irp);
    ULONG out = sl->Parameters.DeviceIoControl.OutputBufferLength;
    PVOID buf = irp->AssociatedIrp.SystemBuffer;
    NTSTATUS st = IoAcquireRemoveLockEx(&x->RemoveLock, irp, __FILE__, __LINE__, sizeof(IO_REMOVE_LOCK));
    ULONG_PTR info = 0;
    if (!NT_SUCCESS(st)) return Complete(irp, st, 0);
    switch (sl->Parameters.DeviceIoControl.IoControlCode) {
    case IOCTL_K2_PING:
        if (out >= sizeof(ULONG)) {
            *(ULONG *)buf = x->Index;
            info = sizeof(ULONG);
        }
        break;
    case IOCTL_K2_RESULT:
        if (out < sizeof(K2_RESULT)) { st = STATUS_BUFFER_TOO_SMALL; break; }
        RtlZeroMemory(buf, sizeof(K2_RESULT));
        RequestTests(x, (K2_RESULT *)buf);
        {
            ULONG len = 0;
            K2_RESULT *r = (K2_RESULT *)buf;
            IoGetDeviceProperty(x->Pdo, DevicePropertyBusNumber, sizeof r->Bus, &r->Bus, &len);
            IoGetDeviceProperty(x->Pdo, DevicePropertyAddress, sizeof r->Address, &r->Address, &len);
        }
        info = sizeof(K2_RESULT);
        break;
    case IOCTL_K2_RAISE: {
        // Raise n interrupts on this device: the number whose DPC ran.
        ULONG n = (sl->Parameters.DeviceIoControl.InputBufferLength >= 4) ? *(ULONG *)buf : 0, i, ok = 0;
        for (i = 0; i < n && i < 64; i++) ok += Raise(x, 1u << (i % 8));
        if (out >= 4) { *(ULONG *)buf = ok; info = 4; }
        break;
    }
    default:
        st = STATUS_INVALID_DEVICE_REQUEST;
    }
    IoReleaseRemoveLockEx(&x->RemoveLock, irp, sizeof(IO_REMOVE_LOCK));
    return Complete(irp, st, info);
}

static NTSTATUS DispatchPower(PDEVICE_OBJECT dev, PIRP irp) {
    EXT *x = (EXT *)dev->DeviceExtension;
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(irp);
    if (sl->MinorFunction == IRP_MN_SET_POWER && sl->Parameters.Power.Type == DevicePowerState && sl->Parameters.Power.State.DeviceState == PowerDeviceD0)
        InterlockedExchange(&x->PowerSeen, 1);
    PoStartNextPowerIrp(irp);
    IoSkipCurrentIrpStackLocation(irp);
    return PoCallDriver(x->Lower, irp);
}

static NTSTATUS DispatchPnp(PDEVICE_OBJECT dev, PIRP irp) {
    EXT *x = (EXT *)dev->DeviceExtension;
    PIO_STACK_LOCATION sl = IoGetCurrentIrpStackLocation(irp);
    NTSTATUS st = IoAcquireRemoveLockEx(&x->RemoveLock, irp, __FILE__, __LINE__, sizeof(IO_REMOVE_LOCK));
    if (!NT_SUCCESS(st)) return Complete(irp, st, 0);
    switch (sl->MinorFunction) {
    case IRP_MN_START_DEVICE:
        // The bus driver starts the device first, then us.
        IoForwardIrpSynchronously(x->Lower, irp);
        st = irp->IoStatus.Status;
        if (NT_SUCCESS(st)) st = Start(x, sl->Parameters.StartDevice.AllocatedResources, sl->Parameters.StartDevice.AllocatedResourcesTranslated);
        DbgPrint("k2edu %u: started %08x, tests %06x\n", x->Index, st, x->Mask);
        IoReleaseRemoveLockEx(&x->RemoveLock, irp, sizeof(IO_REMOVE_LOCK));
        return Complete(irp, st, 0);
    case IRP_MN_REMOVE_DEVICE:
        IoReleaseRemoveLockAndWaitEx(&x->RemoveLock, irp, sizeof(IO_REMOVE_LOCK));
        Stop(x);
        irp->IoStatus.Status = STATUS_SUCCESS;
        IoSkipCurrentIrpStackLocation(irp);
        st = IoCallDriver(x->Lower, irp);
        IoDeleteSymbolicLink(&x->Link);
        if (x->Interface.Buffer) RtlFreeUnicodeString(&x->Interface);
        IoDetachDevice(x->Lower);
        DbgPrint("k2edu %u: removed\n", x->Index);
        InterlockedDecrement(&g_devices);
        IoDeleteDevice(dev);
        return st;
    case IRP_MN_SURPRISE_REMOVAL:
    case IRP_MN_QUERY_REMOVE_DEVICE:
    case IRP_MN_CANCEL_REMOVE_DEVICE:
    case IRP_MN_QUERY_STOP_DEVICE:
    case IRP_MN_CANCEL_STOP_DEVICE:
    case IRP_MN_STOP_DEVICE:
        if (sl->MinorFunction == IRP_MN_SURPRISE_REMOVAL || sl->MinorFunction == IRP_MN_STOP_DEVICE) Stop(x);
        irp->IoStatus.Status = STATUS_SUCCESS;
        break;
    default:
        break;
    }
    IoSkipCurrentIrpStackLocation(irp);
    st = IoCallDriver(x->Lower, irp);
    IoReleaseRemoveLockEx(&x->RemoveLock, irp, sizeof(IO_REMOVE_LOCK));
    return st;
}

// -----| AddDevice, DriverEntry |-----

static NTSTATUS AddDevice(PDRIVER_OBJECT drv, PDEVICE_OBJECT pdo) {
    PDEVICE_OBJECT fdo;
    EXT *x;
    UNICODE_STRING name;
    WCHAR nbuf[32];
    LONG index = InterlockedIncrement(&g_devices) - 1;
    NTSTATUS st;
    RtlInitEmptyUnicodeString(&name, nbuf, sizeof nbuf);
    RtlUnicodeStringPrintf(&name, L"\\Device\\VeilEdu%d", index);
    st = IoCreateDevice(drv, sizeof(EXT), &name, FILE_DEVICE_UNKNOWN, FILE_DEVICE_SECURE_OPEN, FALSE, &fdo);
    if (!NT_SUCCESS(st)) {
        InterlockedDecrement(&g_devices);
        return st;
    }
    x = (EXT *)fdo->DeviceExtension;
    RtlZeroMemory(x, sizeof *x);
    x->Self = fdo;
    x->Pdo = pdo;
    x->Index = (ULONG)index;
    KeInitializeEvent(&x->DpcEvent, SynchronizationEvent, FALSE);
    IoInitializeRemoveLock(&x->RemoveLock, 'udeK', 0, 0);
    IoInitializeDpcRequest(fdo, EduDpc);
    x->Lower = IoAttachDeviceToDeviceStack(fdo, pdo);
    if (!x->Lower) {
        IoDeleteDevice(fdo);
        InterlockedDecrement(&g_devices);
        return STATUS_NO_SUCH_DEVICE;
    }
    if (IoGetAttachedDeviceReference(pdo) == fdo && IoGetDeviceAttachmentBaseRef(fdo) == pdo && x->Lower == pdo && fdo->StackSize == pdo->StackSize + 1)
        x->Mask |= T_ATTACH;
    ObDereferenceObject(fdo);
    ObDereferenceObject(pdo);

    RtlInitEmptyUnicodeString(&x->Link, (PWCHAR)ExAllocatePool2(POOL_FLAG_NON_PAGED, 64, 'udeK'), 64);
    RtlUnicodeStringPrintf(&x->Link, L"\\DosDevices\\VeilEdu%d", index);
    IoCreateSymbolicLink(&x->Link, &name);

    {
        WCHAR hw[256];
        ULONG len = 0, bus = 0xFFFFFFFF, addr = 0xFFFFFFFF;
        BOOLEAN ok = NT_SUCCESS(IoGetDeviceProperty(pdo, DevicePropertyHardwareID, sizeof hw, hw, &len)) && len > 40 &&
                     _wcsnicmp(hw, L"PCI\\VEN_1234&DEV_11E8", 21) == 0;
        ok = ok && NT_SUCCESS(IoGetDeviceProperty(pdo, DevicePropertyBusNumber, sizeof bus, &bus, &len)) && bus < 256;
        ok = ok && NT_SUCCESS(IoGetDeviceProperty(pdo, DevicePropertyAddress, sizeof addr, &addr, &len)) && (addr >> 16) < 32;
        ok = ok && IoGetDeviceProperty(pdo, DevicePropertyHardwareID, 4, hw, &len) == STATUS_BUFFER_TOO_SMALL;
        if (ok) x->Mask |= T_PROPERTY;
        DbgPrint("k2edu %u: %ws at bus %u, device %u, function %u\n", x->Index, hw, bus, addr >> 16, addr & 0xFFFF);
    }
    {
        HANDLE k = NULL, d = NULL;
        UNICODE_STRING v;
        ULONG val = 0xED00 + x->Index;
        UCHAR info[64];
        ULONG got = 0;
        BOOLEAN ok = FALSE;
        RtlInitUnicodeString(&v, L"VeilIndex");
        if (NT_SUCCESS(IoOpenDeviceRegistryKey(pdo, PLUGPLAY_REGKEY_DEVICE, KEY_ALL_ACCESS, &k))) {
            ok = NT_SUCCESS(ZwSetValueKey(k, &v, 0, REG_DWORD, &val, sizeof val)) &&
                 NT_SUCCESS(ZwQueryValueKey(k, &v, KeyValuePartialInformation, info, sizeof info, &got)) &&
                 ((PKEY_VALUE_PARTIAL_INFORMATION)info)->DataLength == 4 && *(ULONG *)((PKEY_VALUE_PARTIAL_INFORMATION)info)->Data == val;
            ZwClose(k);
        }
        ok = ok && NT_SUCCESS(IoOpenDeviceRegistryKey(pdo, PLUGPLAY_REGKEY_DRIVER, KEY_READ, &d));
        if (d) ZwClose(d);
        if (ok) x->Mask |= T_REGKEY;
    }
    {
        ULONG v = 0xC0DE0000 + x->Index, back = 0, need = 0;
        DEVPROPTYPE t = 0;
        if (NT_SUCCESS(IoSetDevicePropertyData(pdo, &KEY_VEIL, LOCALE_NEUTRAL, 0, DEVPROP_TYPE_UINT32, sizeof v, &v)) &&
            NT_SUCCESS(IoGetDevicePropertyData(pdo, &KEY_VEIL, LOCALE_NEUTRAL, 0, sizeof back, &back, &need, &t)) &&
            back == v && t == DEVPROP_TYPE_UINT32 && need == 4)
            x->Mask |= T_PROPDATA;
    }
    {
        LONG ok = NT_SUCCESS(IoAcquireRemoveLock(&x->RemoveLock, (PVOID)1));
        IoReleaseRemoveLock(&x->RemoveLock, (PVOID)1);
        if (ok && x->RemoveLock.Common.IoCount == 1 && !x->RemoveLock.Common.Removed) x->Mask |= T_REMOVELOCK;
    }
    IoRegisterDeviceInterface(pdo, &GUID_VEIL_EDU, NULL, &x->Interface);
    fdo->Flags |= DO_BUFFERED_IO | DO_POWER_PAGABLE;
    fdo->Flags &= ~DO_DEVICE_INITIALIZING;
    DbgPrint("k2edu %u: added, %ws\n", x->Index, x->Interface.Buffer ? x->Interface.Buffer : L"(no interface)");
    return STATUS_SUCCESS;
}

static void Unload(PDRIVER_OBJECT drv) {
    UNREFERENCED_PARAMETER(drv);
    DbgPrint("k2edu: unloaded\n");
}

NTSTATUS DriverEntry(PDRIVER_OBJECT drv, PUNICODE_STRING reg) {
    UNREFERENCED_PARAMETER(reg);
    drv->DriverExtension->AddDevice = AddDevice;
    drv->DriverUnload = Unload;
    drv->MajorFunction[IRP_MJ_CREATE] = DispatchCreateClose;
    drv->MajorFunction[IRP_MJ_CLOSE] = DispatchCreateClose;
    drv->MajorFunction[IRP_MJ_DEVICE_CONTROL] = DispatchControl;
    drv->MajorFunction[IRP_MJ_PNP] = DispatchPnp;
    drv->MajorFunction[IRP_MJ_POWER] = DispatchPower;
    DbgPrint("k2edu: loaded\n");
    return STATUS_SUCCESS;
}
