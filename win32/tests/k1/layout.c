// Every structure offset src/ntos uses, checked against the WDK's headers
// at compile time (build.sh compiles this; it makes no output file worth
// keeping). Keep it in step with the constants in src/ntos/io.rs, ke.rs and
// rtl.rs.
#include <ntddk.h>
#include <stddef.h>

#define AT(type, field, off) static_assert(offsetof(type, field) == (off), #type "." #field)
#define SIZE(type, n) static_assert(sizeof(type) == (n), "sizeof " #type)

// io.rs: DRIVER_OBJECT
SIZE(DRIVER_OBJECT, 0x150);
AT(DRIVER_OBJECT, DeviceObject, 0x08);
AT(DRIVER_OBJECT, DriverStart, 0x18);
AT(DRIVER_OBJECT, DriverSize, 0x20);
AT(DRIVER_OBJECT, DriverExtension, 0x30);
AT(DRIVER_OBJECT, DriverName, 0x38);
AT(DRIVER_OBJECT, DriverInit, 0x58);
AT(DRIVER_OBJECT, DriverUnload, 0x68);
AT(DRIVER_OBJECT, MajorFunction, 0x70);
static_assert(IRP_MJ_MAXIMUM_FUNCTION == 0x1B, "IRP_MJ_MAXIMUM_FUNCTION");
AT(DRIVER_EXTENSION, DriverObject, 0x00);

// io.rs: DEVICE_OBJECT
SIZE(DEVICE_OBJECT, 0x150);
AT(DEVICE_OBJECT, DriverObject, 0x08);
AT(DEVICE_OBJECT, NextDevice, 0x10);
AT(DEVICE_OBJECT, Flags, 0x30);
AT(DEVICE_OBJECT, Characteristics, 0x34);
AT(DEVICE_OBJECT, DeviceExtension, 0x40);
AT(DEVICE_OBJECT, DeviceType, 0x48);
AT(DEVICE_OBJECT, StackSize, 0x4C);
AT(DEVICE_OBJECT, DeviceObjectExtension, 0x138);
AT(DEVOBJ_EXTENSION, DeviceObject, 0x08);
static_assert(DO_DEVICE_INITIALIZING == 0x80, "DO_DEVICE_INITIALIZING");

// io.rs: IRP
SIZE(IRP, 0xD0);
AT(IRP, AssociatedIrp.SystemBuffer, 0x18);
AT(IRP, ThreadListEntry, 0x20);
AT(IRP, IoStatus.Status, 0x30);
AT(IRP, IoStatus.Information, 0x38);
AT(IRP, PendingReturned, 0x41);
AT(IRP, StackCount, 0x42);
AT(IRP, CurrentLocation, 0x43);
AT(IRP, UserIosb, 0x48);
AT(IRP, UserEvent, 0x50);
AT(IRP, UserBuffer, 0x70);
AT(IRP, Tail.Overlay.CurrentStackLocation, 0xB8);

// io.rs: IO_STACK_LOCATION
SIZE(IO_STACK_LOCATION, 0x48);
AT(IO_STACK_LOCATION, MajorFunction, 0x00);
AT(IO_STACK_LOCATION, Control, 0x03);
AT(IO_STACK_LOCATION, Parameters, 0x08);
AT(IO_STACK_LOCATION, Parameters.DeviceIoControl.OutputBufferLength, 0x08);
AT(IO_STACK_LOCATION, Parameters.DeviceIoControl.InputBufferLength, 0x10);
AT(IO_STACK_LOCATION, Parameters.DeviceIoControl.IoControlCode, 0x18);
AT(IO_STACK_LOCATION, DeviceObject, 0x28);
AT(IO_STACK_LOCATION, CompletionRoutine, 0x38);
AT(IO_STACK_LOCATION, Context, 0x40);
static_assert(SL_PENDING_RETURNED == 0x01 && SL_INVOKE_ON_CANCEL == 0x20 &&
              SL_INVOKE_ON_SUCCESS == 0x40 && SL_INVOKE_ON_ERROR == 0x80, "SL_ flags");
static_assert(IRP_MJ_CREATE == 0 && IRP_MJ_CLOSE == 2 && IRP_MJ_DEVICE_CONTROL == 0x0E, "IRP_MJ_");

// io.rs: FILE_OBJECT, and the IRP and stack location fields for it
SIZE(FILE_OBJECT, 0xD8);
AT(FILE_OBJECT, DeviceObject, 0x08);
AT(FILE_OBJECT, FileName, 0x58);
AT(IRP, Tail.Overlay.OriginalFileObject, 0xC0);
AT(IO_STACK_LOCATION, FileObject, 0x30);
AT(IO_STACK_LOCATION, Parameters.DeviceIoControl.Type3InputBuffer, 0x20);
static_assert(IRP_MJ_CLEANUP == 0x12, "IRP_MJ_CLEANUP");
static_assert(METHOD_BUFFERED == 0 && METHOD_NEITHER == 3, "METHOD_");

// dpc.rs: KDPC and KTIMER
SIZE(KDPC, 0x40);
AT(KDPC, DeferredRoutine, 0x18);
AT(KDPC, DeferredContext, 0x20);
AT(KDPC, SystemArgument1, 0x28);
AT(KDPC, SystemArgument2, 0x30);
AT(KDPC, DpcData, 0x38);
SIZE(KTIMER, 0x40);
AT(KTIMER, DueTime, 0x18);
AT(KTIMER, Dpc, 0x30);
AT(KTIMER, Period, 0x3C);

// ps.rs: WORK_QUEUE_ITEM
AT(WORK_QUEUE_ITEM, WorkerRoutine, 0x10);
AT(WORK_QUEUE_ITEM, Parameter, 0x18);

// cm.rs, ob.rs: OBJECT_ATTRIBUTES, key value information
AT(OBJECT_ATTRIBUTES, RootDirectory, 0x08);
AT(OBJECT_ATTRIBUTES, ObjectName, 0x10);
AT(KEY_VALUE_PARTIAL_INFORMATION, Type, 4);
AT(KEY_VALUE_PARTIAL_INFORMATION, DataLength, 8);
AT(KEY_VALUE_PARTIAL_INFORMATION, Data, 12);
AT(KEY_VALUE_FULL_INFORMATION, DataOffset, 8);
AT(KEY_VALUE_FULL_INFORMATION, NameLength, 16);
AT(KEY_VALUE_FULL_INFORMATION, Name, 20);
AT(KEY_VALUE_BASIC_INFORMATION, NameLength, 8);
AT(KEY_VALUE_BASIC_INFORMATION, Name, 12);

// ke.rs: dispatcher objects, queued spinlock handle
SIZE(DISPATCHER_HEADER, 0x18);
AT(DISPATCHER_HEADER, SignalState, 0x04);
AT(DISPATCHER_HEADER, WaitListHead, 0x08);
SIZE(KEVENT, 0x18);
AT(KSEMAPHORE, Limit, 0x18);
SIZE(KMUTANT, 0x38);
AT(KLOCK_QUEUE_HANDLE, LockQueue.Lock, 0x08);
AT(KLOCK_QUEUE_HANDLE, OldIrql, 0x10);
static_assert(NotificationEvent == 0 && SynchronizationEvent == 1, "EVENT_TYPE");

// rtl.rs: counted strings, version
AT(UNICODE_STRING, Length, 0);
AT(UNICODE_STRING, MaximumLength, 2);
AT(UNICODE_STRING, Buffer, 8);
AT(RTL_OSVERSIONINFOW, dwMajorVersion, 4);
AT(RTL_OSVERSIONINFOW, dwBuildNumber, 12);
AT(RTL_OSVERSIONINFOW, szCSDVersion, 20);
SIZE(RTL_OSVERSIONINFOW, 276);
AT(RTL_OSVERSIONINFOEXW, wProductType, 282);
