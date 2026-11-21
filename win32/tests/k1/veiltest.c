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

#define IOCTL_VEIL_ECHO  CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_ADD   CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS)
#define IOCTL_VEIL_STATS CTL_CODE(FILE_DEVICE_UNKNOWN, 0x802, METHOD_BUFFERED, FILE_ANY_ACCESS)

typedef struct _VEIL_EXT {
    KSPIN_LOCK Lock;
    ULONG Creates, Closes, Ioctls;
} VEIL_EXT;

static PVOID g_scratch;
static UNICODE_STRING g_link;

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
    DbgPrint("veiltest: \\Device\\VeilTest ready (stack size %d)\n", dev->StackSize);
    return STATUS_SUCCESS;
}
