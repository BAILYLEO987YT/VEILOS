// Every structure offset src/ntos uses, checked against the WDK's headers
// at compile time (build.sh compiles this; it makes no output file worth
// keeping). Keep it in step with the constants in src/ntos/io.rs, ke.rs and
// rtl.rs.
#include <ntifs.h> // ntddk.h plus the file-system structures (SECURITY_DESCRIPTOR)
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

// mm.rs: MDL
SIZE(MDL, 0x30);
AT(MDL, Size, 0x08);
AT(MDL, MdlFlags, 0x0A);
AT(MDL, Process, 0x10);
AT(MDL, MappedSystemVa, 0x18);
AT(MDL, StartVa, 0x20);
AT(MDL, ByteCount, 0x28);
AT(MDL, ByteOffset, 0x2C);
AT(IRP, MdlAddress, 0x08);
static_assert(MDL_MAPPED_TO_SYSTEM_VA == 0x1 && MDL_PAGES_LOCKED == 0x2 && MDL_SOURCE_IS_NONPAGED_POOL == 0x4 &&
              MDL_PARTIAL == 0x10, "MDL_ flags");
static_assert(METHOD_IN_DIRECT == 1 && METHOD_OUT_DIRECT == 2, "METHOD_ direct");
static_assert(MmNonCached == 0 && MmCached == 1 && MmWriteCombined == 2, "MEMORY_CACHING_TYPE");
static_assert(PAGE_NOCACHE == 0x200 && PAGE_WRITECOMBINE == 0x400, "PAGE_ cache flags");

// ex.rs: fast and guarded mutexes, resources, SLists, lookaside lists
AT(FAST_MUTEX, Count, 0x00);
AT(FAST_MUTEX, Owner, 0x08);
AT(FAST_MUTEX, Contention, 0x10);
AT(FAST_MUTEX, Event, 0x18);
AT(FAST_MUTEX, OldIrql, 0x30);
SIZE(FAST_MUTEX, 0x38);
SIZE(KGUARDED_MUTEX, 0x38);
static_assert(sizeof(ERESOURCE) >= 0x18, "ERESOURCE room for VEILOS's fields");
SIZE(SLIST_HEADER, 0x10);
AT(GENERAL_LOOKASIDE, ListHead, 0x00);
AT(GENERAL_LOOKASIDE, Depth, 0x10);
AT(GENERAL_LOOKASIDE, MaximumDepth, 0x12);
AT(GENERAL_LOOKASIDE, TotalAllocates, 0x14);
AT(GENERAL_LOOKASIDE, AllocateMisses, 0x18);
AT(GENERAL_LOOKASIDE, TotalFrees, 0x1C);
AT(GENERAL_LOOKASIDE, FreeMisses, 0x20);
AT(GENERAL_LOOKASIDE, Type, 0x24);
AT(GENERAL_LOOKASIDE, Tag, 0x28);
AT(GENERAL_LOOKASIDE, Size, 0x2C);
AT(GENERAL_LOOKASIDE, Allocate, 0x30);
AT(GENERAL_LOOKASIDE, Free, 0x38);
AT(GENERAL_LOOKASIDE, ListEntry, 0x40);
AT(NPAGED_LOOKASIDE_LIST, L, 0x00);

// rtlx.rs: bitmaps, generic tables, registry query table, security, resources
AT(RTL_BITMAP, SizeOfBitMap, 0);
AT(RTL_BITMAP, Buffer, 8);
AT(RTL_GENERIC_TABLE, OrderedPointer, 0x18);
AT(RTL_GENERIC_TABLE, NumberGenericTableElements, 0x24);
AT(RTL_GENERIC_TABLE, CompareRoutine, 0x28);
AT(RTL_GENERIC_TABLE, AllocateRoutine, 0x30);
AT(RTL_GENERIC_TABLE, FreeRoutine, 0x38);
AT(RTL_GENERIC_TABLE, TableContext, 0x40);
SIZE(TIME_FIELDS, 16);
AT(TIME_FIELDS, Weekday, 14);
SIZE(RTL_QUERY_REGISTRY_TABLE, 0x38);
AT(RTL_QUERY_REGISTRY_TABLE, Flags, 0x08);
AT(RTL_QUERY_REGISTRY_TABLE, Name, 0x10);
AT(RTL_QUERY_REGISTRY_TABLE, EntryContext, 0x18);
AT(RTL_QUERY_REGISTRY_TABLE, DefaultType, 0x20);
AT(RTL_QUERY_REGISTRY_TABLE, DefaultData, 0x28);
AT(RTL_QUERY_REGISTRY_TABLE, DefaultLength, 0x30);
static_assert(RTL_QUERY_REGISTRY_SUBKEY == 1 && RTL_QUERY_REGISTRY_TOPKEY == 2 && RTL_QUERY_REGISTRY_REQUIRED == 4 &&
              RTL_QUERY_REGISTRY_NOVALUE == 8 && RTL_QUERY_REGISTRY_DIRECT == 0x20, "RTL_QUERY_REGISTRY_");
static_assert(RTL_REGISTRY_ABSOLUTE == 0 && RTL_REGISTRY_SERVICES == 1 && RTL_REGISTRY_CONTROL == 2 &&
              RTL_REGISTRY_WINDOWS_NT == 3 && RTL_REGISTRY_DEVICEMAP == 4 && RTL_REGISTRY_USER == 5, "RTL_REGISTRY_");
AT(SECURITY_DESCRIPTOR, Control, 2);
AT(SECURITY_DESCRIPTOR, Owner, 0x08);
AT(SECURITY_DESCRIPTOR, Group, 0x10);
AT(SECURITY_DESCRIPTOR, Sacl, 0x18);
AT(SECURITY_DESCRIPTOR, Dacl, 0x20);
AT(ACL, AclSize, 2);
AT(ACL, AceCount, 4);
AT(CM_PARTIAL_RESOURCE_DESCRIPTOR, Flags, 2);
AT(CM_PARTIAL_RESOURCE_DESCRIPTOR, u.Memory.Start, 4);
AT(CM_PARTIAL_RESOURCE_DESCRIPTOR, u.Memory.Length, 0xC);
static_assert(CmResourceTypeMemory == 3 && CmResourceTypeMemoryLarge == 7, "CmResourceType");
static_assert(CM_RESOURCE_MEMORY_LARGE_40 == 0x200 && CM_RESOURCE_MEMORY_LARGE_48 == 0x400 &&
              CM_RESOURCE_MEMORY_LARGE_64 == 0x800, "CM_RESOURCE_MEMORY_LARGE");
AT(IO_RESOURCE_DESCRIPTOR, u.Memory.Length, 8);
AT(IO_RESOURCE_DESCRIPTOR, u.Memory.Alignment, 0xC);
AT(IO_RESOURCE_DESCRIPTOR, u.Memory.MinimumAddress, 0x10);
AT(IO_RESOURCE_DESCRIPTOR, u.Memory.MaximumAddress, 0x18);

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
