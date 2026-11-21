// What VEILOS's ntoskrnl.exe and hal.dll export to drivers: name -> address.

use super::{cm, dpc, io, ke, mm, ob, pool, ps, rtl, seh};

// -----| pool |-----

pub extern "win64" fn ExAllocatePoolWithTag(_pool_type: u32, size: usize, _tag: u32) -> *mut u8 {
    pool::alloc_tracked(size)
}

pub extern "win64" fn ExAllocatePool(pool_type: u32, size: usize) -> *mut u8 {
    ExAllocatePoolWithTag(pool_type, size, 0)
}

/// ExAllocatePool2(Flags, Size, Tag): always zeroed (pool memory is).
pub extern "win64" fn ExAllocatePool2(_flags: u64, size: usize, _tag: u32) -> *mut u8 {
    pool::alloc_tracked(size)
}

pub extern "win64" fn ExFreePoolWithTag(p: *mut u8, _tag: u32) {
    pool::free_tracked(p)
}

pub extern "win64" fn ExFreePool(p: *mut u8) {
    pool::free_tracked(p)
}

/// MmGetSystemRoutineAddress(UNICODE_STRING name): drivers look up optional
/// functions this way; null when VEILOS doesn't have one.
pub extern "win64" fn MmGetSystemRoutineAddress(name: *const u8) -> u64 {
    let n = rtl::unicode_to_string(name);
    lookup("ntoskrnl.exe", &n).or_else(|| lookup("hal.dll", &n)).unwrap_or(0)
}

macro_rules! table {
    ($name:expr; $($f:path),* $(,)?) => {{
        let n: &str = $name;
        $(
            if n == stringify!($f).rsplit("::").next().unwrap() {
                return Some($f as *const () as usize as u64);
            }
        )*
    }};
}

/// The address `module` exports as `name`, if VEILOS has it.
pub fn lookup(module: &str, name: &str) -> Option<u64> {
    match module.to_ascii_lowercase().as_str() {
        "ntoskrnl.exe" => {
            table!(name;
                // pool
                ExAllocatePoolWithTag, ExAllocatePool, ExAllocatePool2, ExFreePoolWithTag, ExFreePool,
                MmGetSystemRoutineAddress,
                // IRQL, locks, dispatcher objects, waits
                ke::KeGetCurrentIrql, ke::KfRaiseIrql, ke::KeLowerIrql,
                ke::KeInitializeSpinLock, ke::KeAcquireSpinLockRaiseToDpc, ke::KeReleaseSpinLock,
                ke::KeAcquireSpinLockAtDpcLevel, ke::KeReleaseSpinLockFromDpcLevel, ke::KeTryToAcquireSpinLockAtDpcLevel,
                ke::KeAcquireInStackQueuedSpinLock, ke::KeReleaseInStackQueuedSpinLock,
                ke::KeAcquireInStackQueuedSpinLockAtDpcLevel, ke::KeReleaseInStackQueuedSpinLockFromDpcLevel,
                ke::KeInitializeEvent, ke::KeSetEvent, ke::KeResetEvent, ke::KeClearEvent, ke::KeReadStateEvent,
                ke::KeInitializeSemaphore, ke::KeReleaseSemaphore, ke::KeInitializeMutex, ke::KeReleaseMutex,
                ke::KeWaitForSingleObject, ke::KeWaitForMultipleObjects, ke::KeDelayExecutionThread,
                ke::KeEnterCriticalRegion, ke::KeLeaveCriticalRegion,
                // time, processors, bug checks
                ke::KeQuerySystemTimePrecise, ke::KeQuerySystemTime, ke::ExSystemTimeToLocalTime,
                ke::KeQueryTimeIncrement, ke::KeQueryInterruptTime,
                ke::KeQueryActiveProcessorCount, ke::KeGetCurrentProcessorNumberEx,
                ke::KeBugCheckEx, ke::KeBugCheck,
                // I/O manager
                io::IoCreateDevice, io::IoDeleteDevice, io::IoCreateSymbolicLink, io::IoDeleteSymbolicLink,
                io::IoAllocateIrp, io::IoFreeIrp, io::IofCallDriver, io::IofCompleteRequest,
                io::IoGetCurrentProcess, io::PsGetCurrentProcessId, io::PsGetCurrentThreadId,
                io::IoIsWdmVersionAvailable,
                // run-time library and C runtime
                rtl::RtlInitUnicodeString, rtl::RtlInitAnsiString, rtl::RtlInitString, rtl::RtlCopyUnicodeString,
                rtl::RtlCompareUnicodeString, rtl::RtlEqualUnicodeString, rtl::RtlGetVersion, rtl::RtlCompareMemory,
                rtl::memcpy, rtl::memmove, rtl::memset, rtl::memcmp, rtl::strlen, rtl::wcslen,
                rtl::strcmp, rtl::strncmp, rtl::_stricmp, rtl::wcscmp, rtl::__chkstk,
                rtl::DbgPrint, rtl::DbgPrintEx, rtl::vDbgPrintEx,
                // DPCs, timers, threads, work items
                dpc::KeInitializeDpc, dpc::KeInitializeThreadedDpc, dpc::KeSetImportanceDpc, dpc::KeInsertQueueDpc,
                dpc::KeRemoveQueueDpc, dpc::KeFlushQueuedDpcs,
                dpc::KeInitializeTimer, dpc::KeInitializeTimerEx, dpc::KeSetTimer, dpc::KeSetTimerEx,
                dpc::KeSetCoalescableTimer, dpc::KeCancelTimer, dpc::KeReadStateTimer,
                ps::PsCreateSystemThread, ps::PsTerminateSystemThread, ps::PsIsSystemThread,
                ps::IoSizeofWorkItem, ps::IoAllocateWorkItem, ps::IoInitializeWorkItem, ps::IoUninitializeWorkItem,
                ps::IoFreeWorkItem, ps::IoQueueWorkItem, ps::IoQueueWorkItemEx, ps::ExQueueWorkItem,
                // handles, objects, registry
                ob::ZwClose, ob::ObReferenceObjectByHandle, ob::ObfReferenceObject, ob::ObfDereferenceObject,
                ob::ObDereferenceObjectDeferDelete,
                cm::ZwCreateKey, cm::ZwOpenKey, cm::ZwQueryValueKey, cm::ZwSetValueKey, cm::ZwDeleteValueKey,
                cm::ZwDeleteKey, cm::RtlWriteRegistryValue,
                // structured exception handling
                seh::__C_specific_handler, seh::RtlRaiseException, seh::ExRaiseStatus, seh::RtlUnwindEx, seh::RtlUnwind,
                seh::ProbeForRead, seh::ProbeForWrite, seh::RtlCaptureContext, seh::RtlRestoreContext,
                seh::RtlLookupFunctionEntry, seh::RtlVirtualUnwind, seh::RtlPcToFileHeader,
                // memory: MDLs, contiguous, device, physical
                mm::MmGetPhysicalAddress, mm::MmIsAddressValid, mm::IoAllocateMdl, mm::IoFreeMdl,
                mm::MmBuildMdlForNonPagedPool, mm::MmProbeAndLockPages, mm::MmUnlockPages,
                mm::MmMapLockedPagesSpecifyCache, mm::MmMapLockedPages, mm::MmUnmapLockedPages, mm::IoBuildPartialMdl,
                mm::MmAllocatePagesForMdlEx, mm::MmAllocatePagesForMdl, mm::MmFreePagesFromMdl,
                mm::MmAllocateContiguousMemorySpecifyCache, mm::MmAllocateContiguousMemory, mm::MmAllocateNonCachedMemory,
                mm::MmFreeContiguousMemory, mm::MmFreeContiguousMemorySpecifyCache, mm::MmFreeNonCachedMemory,
                mm::MmMapIoSpace, mm::MmMapIoSpaceEx, mm::MmUnmapIoSpace,
            );
            // Data exports.
            match name {
                "MmUserProbeAddress" => return Some(&raw const seh::MM_USER_PROBE_ADDRESS as u64),
                "MmHighestUserAddress" => return Some(&raw const seh::MM_HIGHEST_USER_ADDRESS as u64),
                "MmSystemRangeStart" => return Some(&raw const seh::MM_SYSTEM_RANGE_START as u64),
                _ => {}
            }
            None
        }
        "hal.dll" => {
            table!(name; ke::KeQueryPerformanceCounter, ke::KeStallExecutionProcessor);
            None
        }
        _ => None,
    }
}
