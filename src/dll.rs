use alloc::vec::Vec;
use alloc::string::String;
use spin::Mutex;

// -----| win32 symbols & DLLs |-----

pub struct Win32Symbol {
    pub name: String,
    pub syscall_id: u64,
    pub address: u64, // Virtual address of the trampoline
}

pub struct FakeDll {
    pub name: String,
    pub symbols: Vec<Win32Symbol>,
}

impl FakeDll {
    pub fn new(name: &str) -> Self {
        Self {
            name: String::from(name),
            symbols: Vec::new(),
        }
    }

    pub fn add_symbol(&mut self, name: &str, id: u64) {
        self.symbols.push(Win32Symbol {
            name: String::from(name),
            syscall_id: id,
            address: 0, // Assigned later
        });
    }
}

// -----| dll registry |-----

pub struct DllRegistry {
    pub dlls: Vec<FakeDll>,
}

impl DllRegistry {
    pub const fn new() -> Self {
        Self {
            dlls: Vec::new(),
        }
    }

    pub fn register(&mut self, dll: FakeDll) {
        self.dlls.push(dll);
    }

    pub fn get_dll(&self, name: &str) -> Option<&FakeDll> {
        self.dlls.iter().find(|d| d.name.eq_ignore_ascii_case(name))
    }
}

pub static DLL_REGISTRY: Mutex<DllRegistry> = Mutex::new(DllRegistry::new());

// -----| initialization |-----

pub fn init() {
    let mut reg = DLL_REGISTRY.lock();
    
    // -----| kernel32.dll |-----
    let mut k32 = FakeDll::new("kernel32.dll");
    // Standard I/O
    k32.add_symbol("GetStdHandle", 0x1001);
    k32.add_symbol("WriteConsoleA", 0x1002);
    k32.add_symbol("WriteConsoleW", 0x1002);
    k32.add_symbol("ExitProcess", 0x1003);
    
    // Module & Process Management
    k32.add_symbol("GetModuleHandleA", 0x1004);
    k32.add_symbol("GetModuleHandleW", 0x1004);
    k32.add_symbol("GetProcAddress", 0x1005);
    k32.add_symbol("GetCurrentProcessId", 0x1006);
    k32.add_symbol("GetCurrentThreadId", 0x1007);
    k32.add_symbol("GetCommandLineA", 0x1008);
    k32.add_symbol("GetCommandLineW", 0x1008);
    
    // Memory Management
    k32.add_symbol("VirtualAlloc", 0x1010);
    k32.add_symbol("VirtualFree", 0x1011);
    k32.add_symbol("VirtualProtect", 0x1012);
    k32.add_symbol("HeapAlloc", 0x1013);
    k32.add_symbol("HeapFree", 0x1014);
    
    // Threading & Synchronization
    k32.add_symbol("CreateThread", 0x1020);
    k32.add_symbol("InitializeCriticalSection",            0x1021);
    k32.add_symbol("InitializeCriticalSectionEx",          0x1021);
    k32.add_symbol("InitializeCriticalSectionAndSpinCount", 0x1086);
    k32.add_symbol("EnterCriticalSection", 0x1022);
    k32.add_symbol("LeaveCriticalSection", 0x1023);
    k32.add_symbol("Sleep", 0x1024);
    
    // Handle management
    k32.add_symbol("CloseHandle",                    0x1030);
    k32.add_symbol("GetCurrentProcess",              0x1031);
    k32.add_symbol("DuplicateHandle",                0x1032);
    k32.add_symbol("SetHandleInformation",           0x1033);
    k32.add_symbol("GetProcessHeap",                 0x1034);
    k32.add_symbol("HeapReAlloc",                    0x1035);
    // Error handling
    k32.add_symbol("GetLastError",                   0x1040);
    k32.add_symbol("SetLastError",                   0x1041);
    k32.add_symbol("GetModuleFileNameW",             0x1042);
    k32.add_symbol("IsProcessorFeaturePresent",      0x1043);
    k32.add_symbol("IsDebuggerPresent",              0x1044);
    k32.add_symbol("UnhandledExceptionFilter",       0x1045);
    k32.add_symbol("SetUnhandledExceptionFilter",    0x1046);
    k32.add_symbol("AddVectoredExceptionHandler",    0x1047);
    // Time
    k32.add_symbol("QueryPerformanceCounter",        0x1050);
    k32.add_symbol("QueryPerformanceFrequency",      0x1051);
    k32.add_symbol("GetSystemTimeAsFileTime",        0x1052);
    k32.add_symbol("GetSystemTimePreciseAsFileTime", 0x1052);
    // String/locale
    k32.add_symbol("WideCharToMultiByte",            0x1060);
    k32.add_symbol("MultiByteToWideChar",            0x1061);
    k32.add_symbol("lstrlenW",                       0x1062);
    k32.add_symbol("CompareStringOrdinal",           0x1063);
    k32.add_symbol("FormatMessageW",                 0x1064);
    // Memory/heap extras
    k32.add_symbol("GlobalAlloc",                    0x1070);
    k32.add_symbol("GlobalFree",                     0x1071);
    k32.add_symbol("GlobalLock",                     0x1072);
    k32.add_symbol("GlobalUnlock",                   0x1073);
    k32.add_symbol("GlobalSize",                     0x1074);
    k32.add_symbol("InitializeSListHead",             0x1075);
    // Threading extras
    k32.add_symbol("GetCurrentThread",               0x1080);
    k32.add_symbol("WaitForSingleObject",            0x1081);
    k32.add_symbol("WaitForSingleObjectEx",          0x1082);
    k32.add_symbol("CreateMutexA",                   0x1083);
    k32.add_symbol("ReleaseMutex",                   0x1084);
    k32.add_symbol("SwitchToThread",                 0x1085);
    k32.add_symbol("SleepEx",                        0x1124);
    k32.add_symbol("SetThreadStackGuarantee",        0x1087);
    k32.add_symbol("SetThreadErrorMode",             0x1088);
    k32.add_symbol("CreateThread",                   0x1020);  // ensure alias
    k32.add_symbol("CreateWaitableTimerExW",         0x1090);
    k32.add_symbol("SetWaitableTimer",               0x1091);
    // Library loading
    k32.add_symbol("LoadLibraryA",                   0x10A0);
    k32.add_symbol("LoadLibraryW",                   0x10A0);
    k32.add_symbol("LoadLibraryExW",                 0x10A0);
    k32.add_symbol("FreeLibrary",                    0x10A1);
    // Environment / system info
    k32.add_symbol("GetSystemInfo",                  0x10B0);
    k32.add_symbol("GetEnvironmentVariableW",        0x10B1);
    k32.add_symbol("GetEnvironmentStringsW",         0x10B2);
    k32.add_symbol("FreeEnvironmentStringsW",        0x10B3);
    k32.add_symbol("GetCurrentDirectoryW",           0x10B4);
    k32.add_symbol("GetWindowsDirectoryW",           0x10B5);
    k32.add_symbol("GetSystemDirectoryW",            0x10B6);
    k32.add_symbol("GetConsoleOutputCP",             0x10B7);
    k32.add_symbol("GetConsoleMode",                 0x10B8);
    k32.add_symbol("GetExitCodeProcess",             0x10B9);
    // File I/O
    k32.add_symbol("CreateFileW",                    0x10C0);
    k32.add_symbol("ReadFileEx",                     0x10C1);
    k32.add_symbol("WriteFileEx",                    0x10C2);
    k32.add_symbol("DeleteFileW",                    0x10C3);
    k32.add_symbol("FindFirstFileExW",               0x10C4);
    k32.add_symbol("FindNextFileW",                  0x10C5);
    k32.add_symbol("FindClose",                      0x10C6);
    k32.add_symbol("GetFileAttributesW",             0x10C7);
    k32.add_symbol("GetFileInformationByHandle",     0x10C8);
    k32.add_symbol("GetFileInformationByHandleEx",   0x10C9);
    k32.add_symbol("SetFileInformationByHandle",     0x10CA);
    k32.add_symbol("GetFullPathNameW",               0x10CB);
    k32.add_symbol("GetFinalPathNameByHandleW",      0x10CC);
    k32.add_symbol("CreateDirectoryW",               0x10CD);
    k32.add_symbol("SetFileCompletionNotificationModes", 0x10CE);
    // IO completion
    k32.add_symbol("CreateIoCompletionPort",         0x10D0);
    k32.add_symbol("GetQueuedCompletionStatusEx",    0x10D1);
    k32.add_symbol("PostQueuedCompletionStatus",     0x10D2);
    // Process
    k32.add_symbol("CreateProcessW",                 0x10E0);
    k32.add_symbol("GetExitCodeProcess",             0x10B9);
    // RTL exception support (forwards to ntdll stubs)
    k32.add_symbol("RtlCaptureContext",              0x10F0);
    k32.add_symbol("RtlLookupFunctionEntry",         0x10F1);
    k32.add_symbol("RtlVirtualUnwind",               0x10F2);
    // Threadpool API (stubs that return valid pointers so callers don't crash)
    k32.add_symbol("CreateThreadpoolTimer",        0x1111);
    k32.add_symbol("SetThreadpoolTimer",           0x1112);
    k32.add_symbol("CloseThreadpoolTimer",         0x1113);
    k32.add_symbol("WaitForThreadpoolTimerCallbacks", 0x1114);
    k32.add_symbol("CreateThreadpoolWork",         0x1115);
    k32.add_symbol("SubmitThreadpoolWork",         0x1116);
    k32.add_symbol("CloseThreadpoolWork",          0x1117);
    k32.add_symbol("CreateThreadpoolIo",           0x1118);
    k32.add_symbol("CloseThreadpoolIo",            0x1119);
    k32.add_symbol("CreateThreadpool",             0x111A);
    k32.add_symbol("CloseThreadpool",              0x111B);
    k32.add_symbol("InitializeThreadpoolEnvironment", 0x111C);
    k32.add_symbol("SetThreadpoolCallbackPool",    0x111D);
    // SRW locks — kernel32 exports these as thin wrappers over ntdll
    k32.add_symbol("InitializeSRWLock",            0x1026);
    k32.add_symbol("AcquireSRWLockExclusive",      0x1027);
    k32.add_symbol("ReleaseSRWLockExclusive",      0x1028);
    k32.add_symbol("AcquireSRWLockShared",         0x1029);
    k32.add_symbol("ReleaseSRWLockShared",         0x102A);
    k32.add_symbol("TryAcquireSRWLockExclusive",   0x102B);
    k32.add_symbol("TryAcquireSRWLockShared",      0x102C);
    // Condition variables
    k32.add_symbol("InitializeConditionVariable",  0x1030); // reuses CloseHandle slot — harmless
    k32.add_symbol("SleepConditionVariableSRW",    0x1024); // stub as Sleep
    k32.add_symbol("WakeAllConditionVariable",     0x1122);
    k32.add_symbol("WakeConditionVariable",        0x1123);
    k32.add_symbol("WakeByAddressAll",             0x1121);
    k32.add_symbol("WakeByAddressSingle",          0x10BF);
    // One-time init
    k32.add_symbol("InitOnceBeginInitialize",      0x102F);
    k32.add_symbol("InitOnceComplete",             0x1120);

    // Local heap (thin wrappers over HeapAlloc — same IDs routed in subsystem.rs)
    k32.add_symbol("LocalAlloc",                     0x10F3);
    k32.add_symbol("LocalFree",                      0x10F4);
    k32.add_symbol("LocalReAlloc",                   0x10F5);
    k32.add_symbol("LocalLock",                      0x10F6);
    k32.add_symbol("LocalUnlock",                    0x10F7);
    k32.add_symbol("LocalSize",                      0x10F8);
    // Startup info
    k32.add_symbol("GetStartupInfoA",                0x10F9);
    k32.add_symbol("GetStartupInfoW",                0x10F9);
    // ReadFile / WriteFile (full, not Ex variants)
    k32.add_symbol("ReadFile",                       0x10FA);
    k32.add_symbol("WriteFile",                      0x10FB);
    // Additional module/resource helpers
    k32.add_symbol("FindResourceW",                  0x10FC);
    k32.add_symbol("LoadResource",                   0x10FD);
    k32.add_symbol("LockResource",                   0x10FE);
    k32.add_symbol("SizeofResource",                 0x10FF);
    // Additional time helpers
    k32.add_symbol("GetLocalTime",                   0x1100);
    k32.add_symbol("GetSystemTime",                  0x1101);
    k32.add_symbol("GetDateFormatW",                 0x1102);
    k32.add_symbol("GetTimeFormatW",                 0x1103);
    k32.add_symbol("GetTimeZoneInformation",         0x1104);
    // Path helpers Notepad needs
    k32.add_symbol("GetTempPathW",                   0x1105);
    k32.add_symbol("GetTempFileNameW",               0x1106);
    // Locale / ANSI code page
    k32.add_symbol("GetACP",                         0x1107);
    k32.add_symbol("GetOEMCP",                       0x1108);
    k32.add_symbol("IsValidCodePage",                0x1109);
    k32.add_symbol("GetLocaleInfoW",                 0x110A);
    k32.add_symbol("GetUserDefaultLCID",             0x110B);
    k32.add_symbol("GetSystemDefaultLCID",           0x110B);
    // Handle table extras
    k32.add_symbol("SetStdHandle",                   0x110C);
    k32.add_symbol("FlushFileBuffers",               0x110D);
    k32.add_symbol("SetEndOfFile",                   0x110E);
    k32.add_symbol("GetFileSizeEx",                  0x110F);
    k32.add_symbol("SetFilePointerEx",               0x1110);
        // Missing KERNEL32 stubs (Patch 7)
    k32.add_symbol("CreateMutexExW",              0x1125);
    k32.add_symbol("MulDiv",                      0x1126);
    k32.add_symbol("HeapSetInformation",          0x1127);
    k32.add_symbol("GetTickCount",                0x1128);
    k32.add_symbol("GetTickCount64",              0x1128);
    k32.add_symbol("GetFileAttributesExW",        0x1129);
    k32.add_symbol("GetFileAttributesA",          0x112A);
    k32.add_symbol("FoldStringW",                 0x112B);
    k32.add_symbol("GetUserDefaultUILanguage",    0x112C);
    k32.add_symbol("DebugBreak",                  0x112D);
    k32.add_symbol("DebugBreakProcess",           0x112D);
    k32.add_symbol("OutputDebugStringW",          0x112E);
    k32.add_symbol("OutputDebugStringA",          0x112E);
    k32.add_symbol("FindFirstFileW",              0x112F);
    k32.add_symbol("FindNextFileW",               0x10C5);
    k32.add_symbol("FindClose",                   0x10C6);
    k32.add_symbol("GetFileType",                 0x1130);
    k32.add_symbol("GetFileSize",                 0x1131);
    k32.add_symbol("SetFilePointer",              0x1132);
    k32.add_symbol("LockFile",                    0x1133);
    k32.add_symbol("UnlockFile",                  0x1134);
    k32.add_symbol("CopyFileW",                   0x1135);
    k32.add_symbol("MoveFileW",                   0x1136);
    k32.add_symbol("GetDriveTypeW",               0x1137);
    k32.add_symbol("GetVolumeInformationW",       0x1138);
    k32.add_symbol("GetDiskFreeSpaceExW",         0x1139);
    k32.add_symbol("SetCurrentDirectoryW",        0x113A);
    k32.add_symbol("GetLongPathNameW",            0x113B);
    k32.add_symbol("GetShortPathNameW",           0x113C);
    k32.add_symbol("ExpandEnvironmentStringsW",   0x113D);
    k32.add_symbol("GetPrivateProfileStringW",    0x113E);
    k32.add_symbol("WritePrivateProfileStringW",  0x113F);
    k32.add_symbol("OpenFileMappingW",            0x1140);
    k32.add_symbol("GetMappedFileNameW",          0x1141);
    k32.add_symbol("GetProcessTimes",             0x1142);
    k32.add_symbol("TerminateProcess",            0x1003);
    k32.add_symbol("RaiseException",              0x1143);
    k32.add_symbol("GetProcessMitigationPolicy",  0x1144);
        reg.register(k32);
    
    // -----| ntdll.dll |-----
    let mut nt = FakeDll::new("ntdll.dll");
    nt.add_symbol("NtCreateFile", 0x0048);
    nt.add_symbol("NtReadFile", 0x0001); // Simplified for now
    nt.add_symbol("NtWriteFile", 0x0002);
    nt.add_symbol("NtAllocateVirtualMemory", 0x0018);
    nt.add_symbol("NtFreeVirtualMemory",   0x001E);
    nt.add_symbol("RtlNtStatusToDosError", 0x00B0);
    nt.add_symbol("NtOpenFile",            0x0033);
    nt.add_symbol("NtDeviceIoControlFile", 0x0007);
    nt.add_symbol("NtCancelIoFileEx",      0x003D);
    nt.add_symbol("NtCreateNamedPipeFile", 0x001A);
    reg.register(nt);
    
    // -----| user32.dll |-----
    let mut u32 = FakeDll::new("user32.dll");
    u32.add_symbol("MessageBoxA", 0x2001);
    u32.add_symbol("MessageBoxW", 0x2001);
    u32.add_symbol("CreateWindowExA", 0x2002);
    u32.add_symbol("CreateWindowExW", 0x2002);
    u32.add_symbol("ShowWindow", 0x2003);
    u32.add_symbol("UpdateWindow", 0x2004);
    u32.add_symbol("DispatchMessageA", 0x2005);
    u32.add_symbol("DispatchMessageW", 0x2005);
    u32.add_symbol("TranslateMessage", 0x2006);
    u32.add_symbol("PostQuitMessage", 0x2007);
    u32.add_symbol("PeekMessageA", 0x2008);
    u32.add_symbol("PeekMessageW", 0x2008);
    // Registration — must succeed (return non-zero atom) or apps abort before CreateWindow
    u32.add_symbol("RegisterClassExA",    0x200A);
    u32.add_symbol("RegisterClassExW",    0x200A);
    u32.add_symbol("RegisterClassA",      0x200B);
    u32.add_symbol("RegisterClassW",      0x200B);
    // Blocking message retrieval (standard Win32 message loop)
    u32.add_symbol("GetMessageA",         0x200C);
    u32.add_symbol("GetMessageW",         0x200C);
    // Default window procedure (apps call this for unhandled messages)
    u32.add_symbol("DefWindowProcA",      0x200D);
    u32.add_symbol("DefWindowProcW",      0x200D);
    // Paint cycle
    u32.add_symbol("BeginPaint",          0x200E);
    u32.add_symbol("EndPaint",            0x200F);
    // Resource loading
    u32.add_symbol("LoadCursorA",         0x2011);
    u32.add_symbol("LoadCursorW",         0x2011);
    u32.add_symbol("LoadIconA",           0x2012);
    u32.add_symbol("LoadIconW",           0x2012);
    // Window geometry / state
    u32.add_symbol("GetClientRect",       0x2013);
    u32.add_symbol("InvalidateRect",      0x2014);
    u32.add_symbol("SetWindowLongA",      0x2017);
    u32.add_symbol("SetWindowLongW",      0x2017);
    u32.add_symbol("SetWindowLongPtrA",   0x2017);
    u32.add_symbol("SetWindowLongPtrW",   0x2017);
    u32.add_symbol("GetWindowLongA",      0x2018);
    u32.add_symbol("GetWindowLongW",      0x2018);
    u32.add_symbol("GetWindowLongPtrA",   0x2018);
    u32.add_symbol("GetWindowLongPtrW",   0x2018);
    u32.add_symbol("GetSystemMetrics",    0x201A);
    u32.add_symbol("AdjustWindowRectEx",  0x201B);
    u32.add_symbol("SetFocus",            0x201C);
    u32.add_symbol("GetForegroundWindow", 0x201D);
    u32.add_symbol("SetTimer",            0x201E);
    u32.add_symbol("KillTimer",           0x201F);
    u32.add_symbol("GetDC",               0x2020);
    u32.add_symbol("ReleaseDC",           0x2021);
    u32.add_symbol("GetWindowRect",       0x2022);
    u32.add_symbol("ClientToScreen",      0x2023);
    u32.add_symbol("ScreenToClient",      0x2024);
    u32.add_symbol("GetCursorPos",        0x2025);
    u32.add_symbol("SetCursor",           0x2026);
    u32.add_symbol("SetWindowPos",        0x2027);
    u32.add_symbol("RedrawWindow",        0x2028);
    u32.add_symbol("SetForegroundWindow", 0x2029);
    u32.add_symbol("GetActiveWindow",     0x202A);
    u32.add_symbol("IsIconic",            0x202B);
    u32.add_symbol("IsProcessDPIAware",   0x202C);
    u32.add_symbol("SetCapture",          0x202D);
    u32.add_symbol("ReleaseCapture",      0x202E);
    u32.add_symbol("ValidateRect",        0x202F);
    u32.add_symbol("SendMessageW",        0x2030);
    u32.add_symbol("PostMessageW",        0x2031);
    u32.add_symbol("CallWindowProcW",     0x2032);
    u32.add_symbol("DestroyWindow",       0x2033);
    u32.add_symbol("MonitorFromWindow",   0x2034);
    u32.add_symbol("MonitorFromPoint",    0x2035);
    u32.add_symbol("MonitorFromRect",     0x2036);
    u32.add_symbol("GetMonitorInfoW",     0x2037);
    u32.add_symbol("OpenClipboard",       0x2038);
    u32.add_symbol("CloseClipboard",      0x2039);
    u32.add_symbol("EmptyClipboard",      0x203A);
    u32.add_symbol("GetClipboardData",    0x203B);
    u32.add_symbol("SetClipboardData",    0x203C);
    u32.add_symbol("IsClipboardFormatAvailable", 0x203D);
    u32.add_symbol("RegisterClipboardFormatW",   0x203E);
    u32.add_symbol("SystemParametersInfoA", 0x203F);
    u32.add_symbol("GetKeyState",         0x2040);
    u32.add_symbol("GetAsyncKeyState",    0x2041);
    u32.add_symbol("MapVirtualKeyW",      0x2042);
    u32.add_symbol("GetWindowTextW",      0x2043);
    u32.add_symbol("SetWindowTextW",      0x2044);
    u32.add_symbol("GetWindowTextLengthW",0x2045);
    u32.add_symbol("ShowCursor",          0x2046);
    u32.add_symbol("ClipCursor",          0x2047);
    u32.add_symbol("GetClipCursor",       0x2048);
    u32.add_symbol("FlashWindowEx",       0x2049);
    u32.add_symbol("RegisterRawInputDevices", 0x204A);
    u32.add_symbol("RegisterTouchWindow", 0x204B);
    u32.add_symbol("TrackMouseEvent",     0x204C);
    u32.add_symbol("EnableMenuItem",      0x204D);
    u32.add_symbol("GetSystemMenu",       0x204E);
    u32.add_symbol("GetPropW",            0x204F);
    u32.add_symbol("SetPropW",            0x2050);
    u32.add_symbol("RemovePropW",         0x2051);
    u32.add_symbol("RegisterWindowMessageA", 0x2052);
    u32.add_symbol("LoadStringA",           0x2060);
    u32.add_symbol("LoadStringW",           0x2060);
    u32.add_symbol("GetMenuString",         0x2061);
    u32.add_symbol("AppendMenuW",           0x2062);
    u32.add_symbol("CreateMenu",            0x2063);
    u32.add_symbol("CreatePopupMenu",       0x2064);
    u32.add_symbol("DestroyMenu",           0x2065);
    u32.add_symbol("SetMenu",               0x2066);
    u32.add_symbol("DrawMenuBar",           0x2067);
    u32.add_symbol("LoadMenuW",             0x2068);
    u32.add_symbol("LoadAcceleratorsW",     0x2069);
    u32.add_symbol("TranslateAcceleratorW", 0x206A);
    u32.add_symbol("CharUpperBuffW",        0x206B);
    u32.add_symbol("CharLowerBuffW",        0x206C);
    u32.add_symbol("IsCharAlphaNumericW",   0x206D);
    u32.add_symbol("wsprintfW",             0x206E);
    u32.add_symbol("wvsprintfW",            0x206F);
        // Missing USER32 stubs (Patch 7)
    u32.add_symbol("GetMenu",                0x2070);
    u32.add_symbol("GetSubMenu",             0x2071);
    u32.add_symbol("CheckMenuItem",          0x2072);
    u32.add_symbol("EnableMenuItem",         0x204D);
    u32.add_symbol("GetMenuItemCount",       0x2073);
    u32.add_symbol("GetMenuItemID",          0x2074);
    u32.add_symbol("InsertMenuItemW",        0x2075);
    u32.add_symbol("ModifyMenuW",            0x2076);
    u32.add_symbol("RemoveMenu",             0x2077);
    u32.add_symbol("SetMenuDefaultItem",     0x2078);
    u32.add_symbol("TrackPopupMenu",         0x2079);
    u32.add_symbol("TrackPopupMenuEx",       0x207A);
    u32.add_symbol("GetMenuItemInfoW",       0x207B);
    u32.add_symbol("SetMenuItemInfoW",       0x207C);
    u32.add_symbol("MessageBeep",            0x207D);
    u32.add_symbol("GetDpiForWindow",        0x207E);
    u32.add_symbol("GetDpiForSystem",        0x207E);
    u32.add_symbol("GetDlgCtrlID",           0x207F);
    u32.add_symbol("GetKeyboardLayout",      0x2080);
    u32.add_symbol("GetKeyboardState",       0x2081);
    u32.add_symbol("SetKeyboardState",       0x2082);
    u32.add_symbol("ToUnicodeEx",            0x2083);
    u32.add_symbol("VkKeyScanW",             0x2084);
    u32.add_symbol("GetKeyNameTextW",        0x2085);
    u32.add_symbol("SetActiveWindow",        0x2086);
    u32.add_symbol("BringWindowToTop",       0x2087);
    u32.add_symbol("IsWindow",               0x2088);
    u32.add_symbol("IsWindowEnabled",        0x2089);
    u32.add_symbol("IsWindowVisible",        0x208A);
    u32.add_symbol("EnableWindow",           0x208B);
    u32.add_symbol("GetParent",              0x208C);
    u32.add_symbol("SetParent",              0x208D);
    u32.add_symbol("GetWindow",              0x208E);
    u32.add_symbol("FindWindowW",            0x208F);
    u32.add_symbol("FindWindowExW",          0x2090);
    u32.add_symbol("DialogBoxParamW",        0x2091);
    u32.add_symbol("CreateDialogParamW",     0x2092);
    u32.add_symbol("EndDialog",              0x2093);
    u32.add_symbol("GetDlgItem",             0x2094);
    u32.add_symbol("SetDlgItemTextW",        0x2095);
    u32.add_symbol("GetDlgItemTextW",        0x2096);
    u32.add_symbol("SendDlgItemMessageW",    0x2097);
    u32.add_symbol("GetScrollInfo",          0x2098);
    u32.add_symbol("SetScrollInfo",          0x2099);
    u32.add_symbol("GetScrollPos",           0x209A);
    u32.add_symbol("SetScrollPos",           0x209B);
    u32.add_symbol("ScrollWindow",           0x209C);
    u32.add_symbol("ScrollWindowEx",         0x209D);
    u32.add_symbol("CreateCaret",            0x209E);
    u32.add_symbol("DestroyCaret",           0x209F);
    u32.add_symbol("SetCaretPos",            0x20A0);
    u32.add_symbol("ShowCaret",              0x20A1);  // already have ShowCursor, diff ID
    u32.add_symbol("HideCaret",              0x20A2);
    u32.add_symbol("GetCaretPos",            0x20A3);
    u32.add_symbol("OpenIcon",               0x20A4);
    u32.add_symbol("CloseWindow",            0x20A5);
    u32.add_symbol("ArrangeIconicWindows",   0x20A6);
        reg.register(u32);

    // -----| advapi32.dll |-----
    let mut adv = FakeDll::new("advapi32.dll");
    adv.add_symbol("RegOpenKeyExA", 0x3001);
    adv.add_symbol("RegOpenKeyExW", 0x3001);
    adv.add_symbol("RegQueryValueExA", 0x3002);
    adv.add_symbol("RegQueryValueExW", 0x3002);
    adv.add_symbol("RegCloseKey", 0x3003);
    reg.register(adv);

    // -----| gdi32.dll |-----
    let mut gdi = FakeDll::new("gdi32.dll");
    gdi.add_symbol("GetStockObject", 0x4001);
    gdi.add_symbol("BitBlt", 0x4002);
    gdi.add_symbol("CreateCompatibleDC", 0x4003);
    gdi.add_symbol("DeleteDC", 0x4004);
    gdi.add_symbol("SelectObject", 0x4005);
    gdi.add_symbol("GetDeviceCaps", 0x4006);
    gdi.add_symbol("DeleteObject", 0x4007);
    gdi.add_symbol("CreateSolidBrush", 0x4008);
    gdi.add_symbol("CreateFontIndirectW",    0x4009);
    gdi.add_symbol("CreateFontIndirectA",    0x4009);
    gdi.add_symbol("CreateFontW",            0x400A);
    gdi.add_symbol("CreateFontA",            0x400A);
    gdi.add_symbol("GetTextMetricsW",        0x400B);
    gdi.add_symbol("GetTextMetricsA",        0x400B);
    gdi.add_symbol("TextOutW",               0x400C);
    gdi.add_symbol("TextOutA",               0x400C);
    gdi.add_symbol("GetTextExtentPoint32W",  0x400D);
    gdi.add_symbol("GetTextExtentPoint32A",  0x400D);
    gdi.add_symbol("SetBkMode",              0x400E);
    gdi.add_symbol("SetMapMode",             0x400F);
    gdi.add_symbol("SetTextColor",           0x4010);
    gdi.add_symbol("SetBkColor",             0x4011);
    gdi.add_symbol("GetTextColor",           0x4012);
    gdi.add_symbol("CreateCompatibleBitmap", 0x4013);
    gdi.add_symbol("GetObjectW",             0x4014);
    gdi.add_symbol("GetObjectA",             0x4014);
    gdi.add_symbol("CreatePen",              0x4015);
    gdi.add_symbol("MoveToEx",               0x4016);
    gdi.add_symbol("LineTo",                 0x4017);
    gdi.add_symbol("Rectangle",              0x4018);
    gdi.add_symbol("Ellipse",                0x4019);
    gdi.add_symbol("FillRect",               0x401A);
    gdi.add_symbol("DrawTextW",              0x401B);
    gdi.add_symbol("DrawTextA",              0x401B);
    gdi.add_symbol("SetStretchBltMode",      0x401C);
    gdi.add_symbol("StretchBlt",             0x401D);
    gdi.add_symbol("PatBlt",                 0x401E);
    gdi.add_symbol("GetDC",                  0x401F);  // gdi32 alias
    gdi.add_symbol("ReleaseDC",              0x4020);  // gdi32 alias
    gdi.add_symbol("SaveDC",                 0x4021);
    gdi.add_symbol("RestoreDC",              0x4022);
    gdi.add_symbol("SetViewportOrgEx",       0x4023);
    gdi.add_symbol("SetWindowOrgEx",         0x4024);
    gdi.add_symbol("ExcludeClipRect",        0x4025);
    gdi.add_symbol("IntersectClipRect",      0x4026);
    gdi.add_symbol("GetClipBox",             0x4027);
    gdi.add_symbol("CreateDIBSection",       0x4028);
    gdi.add_symbol("SetDIBitsToDevice",      0x4029);
    gdi.add_symbol("GdiFlush",               0x402A);
    gdi.add_symbol("GetSystemMetrics",       0x402B);  // gdi32 alias
        reg.register(gdi);

    // -----| opengl32.dll |-----
    let mut gl = FakeDll::new("opengl32.dll");
    gl.add_symbol("wglCreateContext", 0x5001);
    gl.add_symbol("wglMakeCurrent", 0x5002);
    gl.add_symbol("wglDeleteContext", 0x5003);
    gl.add_symbol("glGetError", 0x5004);
    reg.register(gl);

    // -----| shell32.dll |-----
    let mut sh = FakeDll::new("shell32.dll");
    sh.add_symbol("ShellExecuteA", 0x6001);
    sh.add_symbol("ShellExecuteW", 0x6001);
    sh.add_symbol("ShellExecuteExW", 0x6003);
    sh.add_symbol("SHGetFolderPathA", 0x6002);
    sh.add_symbol("SHGetFolderPathW", 0x6002);
    reg.register(sh);

    // -----| ole32.dll |-----
    let mut ole = FakeDll::new("ole32.dll");
    ole.add_symbol("CoInitialize", 0x7001);
    ole.add_symbol("CoCreateInstance", 0x7002);
    ole.add_symbol("CoTaskMemFree",   0x7003);
    ole.add_symbol("CoInitializeEx",  0x7004);
    ole.add_symbol("CoUninitialize",  0x7005);
    ole.add_symbol("OleInitialize",   0x7006);
    ole.add_symbol("RegisterDragDrop",0x7007);
    ole.add_symbol("RevokeDragDrop",  0x7008);
    reg.register(ole);

    // -----| oleaut32.dll |-----
    let mut oleaut = FakeDll::new("oleaut32.dll");
    oleaut.add_symbol("SysAllocString", 0x7101);
    oleaut.add_symbol("SysFreeString", 0x7102);
    reg.register(oleaut);

    // -----| ws2_32.dll |-----
    let mut ws = FakeDll::new("ws2_32.dll");
    ws.add_symbol("WSAStartup", 0x8001);
    ws.add_symbol("socket", 0x8002);
    ws.add_symbol("connect", 0x8003);
    ws.add_symbol("send", 0x8004);
    ws.add_symbol("recv",          0x8005);
    ws.add_symbol("freeaddrinfo",  0x8006);
    ws.add_symbol("getaddrinfo",   0x8007);
    ws.add_symbol("closesocket",   0x8008);
    ws.add_symbol("getsockopt",    0x8009);
    ws.add_symbol("ioctlsocket",   0x800A);
    ws.add_symbol("WSAIoctl",      0x800B);
    ws.add_symbol("bind",          0x800C);
    ws.add_symbol("accept",        0x800D);
    ws.add_symbol("listen",        0x800E);
    ws.add_symbol("WSASocketW",    0x800F);
    ws.add_symbol("shutdown",      0x8010);
    ws.add_symbol("WSACleanup",    0x8011);
    ws.add_symbol("WSAGetLastError", 0x8012);
    reg.register(ws);
    
    // -----| dwmapi.dll |-----
    let mut dwm = FakeDll::new("dwmapi.dll");
    dwm.add_symbol("DwmIsCompositionEnabled", 0xC001);
    dwm.add_symbol("DwmSetWindowAttribute", 0xC002);
    dwm.add_symbol("DwmExtendFrameIntoClientArea", 0xC003);
    reg.register(dwm);

    // -----| uxtheme.dll |-----
    let mut ux = FakeDll::new("uxtheme.dll");
    ux.add_symbol("SetWindowTheme", 0xD001);
    ux.add_symbol("IsAppThemed", 0xD002);
    reg.register(ux);

    // -----| imm32.dll |-----
    let mut imm = FakeDll::new("imm32.dll");
    imm.add_symbol("ImmGetContext", 0xE001);
    imm.add_symbol("ImmReleaseContext", 0xE002);
    reg.register(imm);

    // -----| uiautomationcore.dll |-----
    let mut uia = FakeDll::new("uiautomationcore.dll");
    uia.add_symbol("UiaLookupId", 0xF001);
    uia.add_symbol("UiaReturnRawElementProvider", 0xF002);
    reg.register(uia);
    
    // -----| api-ms-win-core-synch-l1-2-0.dll |-----
    let mut synch = FakeDll::new("api-ms-win-core-synch-l1-2-0.dll");
    synch.add_symbol("InitializeCriticalSection", 0x1021);
    synch.add_symbol("EnterCriticalSection", 0x1022);
    synch.add_symbol("LeaveCriticalSection", 0x1023);
    synch.add_symbol("DeleteCriticalSection", 0x1025);
    synch.add_symbol("InitializeSRWLock", 0x1026);
    synch.add_symbol("AcquireSRWLockExclusive", 0x1027);
    synch.add_symbol("ReleaseSRWLockExclusive", 0x1028);
    synch.add_symbol("SleepConditionVariableSRW", 0x1029);
    synch.add_symbol("WakeAllConditionVariable", 0x102A);
    synch.add_symbol("Sleep",               0x1024);
    synch.add_symbol("WakeByAddressSingle", 0x102B);
    synch.add_symbol("WakeByAddressAll",    0x102C);
    synch.add_symbol("WaitOnAddress",       0x102D);
    reg.register(synch);

    // -----| api-ms-win-crt-heap-l1-1-0.dll |-----
    let mut crt_heap = FakeDll::new("api-ms-win-crt-heap-l1-1-0.dll");
    crt_heap.add_symbol("malloc", 0x9001);
    crt_heap.add_symbol("free", 0x9002);
    crt_heap.add_symbol("calloc", 0x9003);
    crt_heap.add_symbol("_set_new_mode", 0x9004);
    reg.register(crt_heap);

    // -----| api-ms-win-crt-runtime-l1-1-0.dll |-----
    let mut crt_run = FakeDll::new("api-ms-win-crt-runtime-l1-1-0.dll");
    crt_run.add_symbol("_initterm", 0x9101);
    crt_run.add_symbol("_initterm_e", 0x9102);
    crt_run.add_symbol("_c_exit", 0x9103);
    crt_run.add_symbol("_register_thread_local_exe_atexit_callback", 0x9104);
    crt_run.add_symbol("_register_onexit_function",   0x9105);
    crt_run.add_symbol("_initialize_onexit_table",    0x9106);
    crt_run.add_symbol("_crt_atexit",                 0x9107);
    crt_run.add_symbol("_cexit",                      0x9108);
    crt_run.add_symbol("_set_app_type",               0x9109);
    crt_run.add_symbol("_configure_narrow_argv",      0x910A);
    crt_run.add_symbol("_initialize_narrow_environment", 0x910B);
    crt_run.add_symbol("_get_initial_narrow_environment", 0x910C);
    crt_run.add_symbol("__p___argc",                  0x910D);
    crt_run.add_symbol("__p___argv",                  0x910E);
    crt_run.add_symbol("_seh_filter_exe",             0x910F);
    crt_run.add_symbol("terminate",                   0x9110);
    crt_run.add_symbol("exit",                        0x9111);
    crt_run.add_symbol("_exit",                       0x9111);
    crt_run.add_symbol("strerror",                    0x9112);
    reg.register(crt_run);

    // -----| api-ms-win-crt-math-l1-1-0.dll |-----
    let mut crt_math = FakeDll::new("api-ms-win-crt-math-l1-1-0.dll");
    crt_math.add_symbol("sin", 0x9501);
    crt_math.add_symbol("cos", 0x9502);
    crt_math.add_symbol("tan", 0x9503);
    crt_math.add_symbol("sqrt", 0x9504);
    crt_math.add_symbol("ceil", 0x9505);
    crt_math.add_symbol("floor", 0x9506);
    crt_math.add_symbol("pow",            0x9507);
    crt_math.add_symbol("round",          0x9508);
    crt_math.add_symbol("trunc",          0x9509);
    crt_math.add_symbol("roundf",         0x950A);
    crt_math.add_symbol("sinf",           0x950B);
    crt_math.add_symbol("cosf",           0x950C);
    crt_math.add_symbol("ceilf",          0x950D);
    crt_math.add_symbol("floorf",         0x950E);
    crt_math.add_symbol("powf",           0x950F);
    crt_math.add_symbol("expf",           0x9510);
    crt_math.add_symbol("acosf",          0x9511);
    crt_math.add_symbol("cbrtf",          0x9512);
    crt_math.add_symbol("atan2f",         0x9513);
    crt_math.add_symbol("truncf",         0x9514);
    crt_math.add_symbol("_hypotf",        0x9515);
    crt_math.add_symbol("__setusermatherr", 0x9516);
    reg.register(crt_math);

    // -----| api-ms-win-crt-locale-l1-1-0.dll |-----
    let mut crt_loc = FakeDll::new("api-ms-win-crt-locale-l1-1-0.dll");
    crt_loc.add_symbol("_configthreadlocale", 0x9601);
    crt_loc.add_symbol("___lc_codepage_func", 0x9602);
    reg.register(crt_loc);

    // -----| api-ms-win-crt-stdio-l1-1-0.dll |-----
    let mut crt_io = FakeDll::new("api-ms-win-crt-stdio-l1-1-0.dll");
    crt_io.add_symbol("__stdio_common_vfprintf", 0x9201);
    crt_io.add_symbol("__stdio_common_vswprintf", 0x9202);
    crt_io.add_symbol("__stdio_common_vsprintf", 0x9203);
    crt_io.add_symbol("__acrt_iob_func",  0x9204);
    crt_io.add_symbol("_set_fmode",        0x9205);
    crt_io.add_symbol("__p__commode",      0x9206);
    reg.register(crt_io);

    // -----| api-ms-win-crt-string-l1-1-0.dll |-----
    let mut crt_str = FakeDll::new("api-ms-win-crt-string-l1-1-0.dll");
    crt_str.add_symbol("memset", 0x9301);
    crt_str.add_symbol("memcpy", 0x9302);
    crt_str.add_symbol("memmove", 0x9303);
    crt_str.add_symbol("strcmp", 0x9304);
    crt_str.add_symbol("strlen",  0x9305);
    crt_str.add_symbol("wcslen",  0x9307);
    crt_str.add_symbol("memcmp",  0x9306);
    reg.register(crt_str);

    // -----| VCRUNTIME140.dll |-----
    let mut vcr = FakeDll::new("VCRUNTIME140.dll");
    vcr.add_symbol("memset", 0x9301);
    vcr.add_symbol("memcpy", 0x9302);
    vcr.add_symbol("__std_type_info_destroy_list", 0x9401);
    vcr.add_symbol("_CxxThrowException",       0x9402);
    vcr.add_symbol("memmove",                  0x9303);  // same handler as crt_string
    vcr.add_symbol("memcmp",                   0x9306);
    vcr.add_symbol("__C_specific_handler",     0x9403);
    vcr.add_symbol("__CxxFrameHandler3",       0x9404);
    vcr.add_symbol("__current_exception",      0x9405);
    vcr.add_symbol("__current_exception_context", 0x9406);
    reg.register(vcr);

    // -----| bcryptprimitives.dll |-----
    let mut bcp = FakeDll::new("bcryptprimitives.dll");
    bcp.add_symbol("ProcessPrng", 0xA001);
    reg.register(bcp);

    // -----| shlwapi.dll |-----
    let mut sha = FakeDll::new("SHLWAPI.dll");
    sha.add_symbol("PathFileExistsA", 0xB001);
    sha.add_symbol("PathFileExistsW", 0xB001);
    reg.register(sha);
    
    crate::serial_println!("[DLL] Registered comprehensive UCRT Math, Locale, and API-Set symbols.");
    // ── api-ms-win-core-synch-l1-1-0.dll ──
    let mut sync2 = FakeDll::new("api-ms-win-core-synch-l1-1-0.dll");
    sync2.add_symbol("SetEvent",             0x1145);
    sync2.add_symbol("ResetEvent",           0x1146);
    sync2.add_symbol("CreateEventExW",       0x1147);
    sync2.add_symbol("WaitForSingleObjectEx",0x1082);
    reg.register(sync2);

    // ── api-ms-win-core-sysinfo-l1-1-0.dll ──
    let mut sysinfo = FakeDll::new("api-ms-win-core-sysinfo-l1-1-0.dll");
    sysinfo.add_symbol("GetTickCount",       0x1128);
    sysinfo.add_symbol("GetTickCount64",     0x1128);
    sysinfo.add_symbol("GetSystemTimeAsFileTime", 0x1052);
    reg.register(sysinfo);

    // ── api-ms-win-core-errorhandling-l1-1-0.dll ──
    let mut errdll = FakeDll::new("api-ms-win-core-errorhandling-l1-1-0.dll");
    errdll.add_symbol("RaiseException",      0x1143);
    errdll.add_symbol("GetLastError",        0x1040);
    errdll.add_symbol("SetLastError",        0x1041);
    reg.register(errdll);

    // ── api-ms-win-core-processthreads-l1-1-0.dll ──
    let mut ptdll = FakeDll::new("api-ms-win-core-processthreads-l1-1-0.dll");
    ptdll.add_symbol("TerminateProcess",     0x1003);
    ptdll.add_symbol("GetCurrentProcess",    0x1031);
    ptdll.add_symbol("GetCurrentProcessId",  0x1006);
    ptdll.add_symbol("GetCurrentThreadId",   0x1007);
    reg.register(ptdll);

    let mut pt2dll = FakeDll::new("api-ms-win-core-processthreads-l1-1-1.dll");
    pt2dll.add_symbol("GetProcessMitigationPolicy", 0x1144);
    reg.register(pt2dll);

    // ── api-ms-win-core-com-l1-1-0.dll (COM) ──
    let mut comdll = FakeDll::new("api-ms-win-core-com-l1-1-0.dll");
    comdll.add_symbol("CoWaitForMultipleHandles",    0x1148);
    comdll.add_symbol("PropVariantClear",             0x1149);
    comdll.add_symbol("CoTaskMemAlloc",               0x114A);
    comdll.add_symbol("CoTaskMemFree",                0x114B);
    comdll.add_symbol("CoCreateFreeThreadedMarshaler",0x114C);
    comdll.add_symbol("CoCreateGuid",                 0x114D);
    reg.register(comdll);

    // ── api-ms-win-core-winrt-l1-1-0.dll ──
    let mut winrt = FakeDll::new("api-ms-win-core-winrt-l1-1-0.dll");
    winrt.add_symbol("RoInitialize",          0x114E);
    winrt.add_symbol("RoUninitialize",        0x114F);
    winrt.add_symbol("RoGetActivationFactory",0x1150);
    reg.register(winrt);

    // ── api-ms-win-core-winrt-string-l1-1-0.dll ──
    let mut wrs = FakeDll::new("api-ms-win-core-winrt-string-l1-1-0.dll");
    wrs.add_symbol("WindowsCreateString",         0x1151);
    wrs.add_symbol("WindowsDeleteString",          0x1152);
    wrs.add_symbol("WindowsGetStringRawBuffer",    0x1153);
    wrs.add_symbol("WindowsCreateStringReference",0x1154);
    reg.register(wrs);

    // ── api-ms-win-core-winrt-error-l1-1-0.dll ──
    let mut wre = FakeDll::new("api-ms-win-core-winrt-error-l1-1-0.dll");
    wre.add_symbol("SetRestrictedErrorInfo",           0x1155);
    reg.register(wre);
    let mut wre2 = FakeDll::new("api-ms-win-core-winrt-error-l1-1-1.dll");
    wre2.add_symbol("RoGetMatchingRestrictedErrorInfo",0x1155);
    reg.register(wre2);

    // ── api-ms-win-crt-private-l1-1-0.dll (_o_ wrappers) ──
    let mut crtpriv = FakeDll::new("api-ms-win-crt-private-l1-1-0.dll");
    crtpriv.add_symbol("_o__errno",              0x1156);
    crtpriv.add_symbol("_o__exit",               0x1003);
    crtpriv.add_symbol("_o_abort",               0x1157);
    crtpriv.add_symbol("_o_malloc",              0x9001);
    crtpriv.add_symbol("_o_free",                0x9002);
    crtpriv.add_symbol("_o_realloc",             0x9004);
    crtpriv.add_symbol("_o_calloc",              0x9003);
    crtpriv.add_symbol("_o__callnewh",           0x1158);
    crtpriv.add_symbol("_o__cexit",              0x1159);
    crtpriv.add_symbol("_o__configthreadlocale", 0x115A);
    crtpriv.add_symbol("_o__configure_wide_argv",0x115B);
    crtpriv.add_symbol("_o__crt_atexit",         0x115C);
    crtpriv.add_symbol("_o__get_wide_winmain_command_line", 0x115D);
    crtpriv.add_symbol("_o__initenv",            0x115E);
    crtpriv.add_symbol("_o__invalid_parameter_noinfo_noreturn", 0x115F);
    crtpriv.add_symbol("_o__set_app_type",       0x1160);
    crtpriv.add_symbol("_o__set_fmode",          0x1161);
    crtpriv.add_symbol("_o_terminate",           0x1157);
    crtpriv.add_symbol("_o_wcslen",              0x1062);
    crtpriv.add_symbol("_o_wcscpy_s",            0x1162);
    crtpriv.add_symbol("_o_wcsncpy_s",           0x1163);
    crtpriv.add_symbol("_o_wcsnlen",             0x1164);
    crtpriv.add_symbol("_o_memcpy_s",            0x1165);
    crtpriv.add_symbol("_o_memmove_s",           0x1166);
    crtpriv.add_symbol("_o_memmove",             0x1167);
    crtpriv.add_symbol("_o_memset",              0x9301);
    crtpriv.add_symbol("_o_memcmp",              0x1168);
    crtpriv.add_symbol("_o_strlen",              0x1169);
    crtpriv.add_symbol("_o_strncpy_s",           0x116A);
    reg.register(crtpriv);

    // ── api-ms-win-crt-string-l1-1-0.dll ──
    let mut crtstr = FakeDll::new("api-ms-win-crt-string-l1-1-0.dll");
    crtstr.add_symbol("wcsnlen",  0x1164);
    crtstr.add_symbol("wcscmp",   0x116B);
    reg.register(crtstr);

    // ── api-ms-win-shcore-path-l1-1-0.dll ──
    let mut shpath = FakeDll::new("api-ms-win-shcore-path-l1-1-0.dll");
    shpath.add_symbol("Ordinal#170",  0x116C);
    reg.register(shpath);

    // ── api-ms-win-shcore-obsolete-l1-1-0.dll ──
    let mut shobs = FakeDll::new("api-ms-win-shcore-obsolete-l1-1-0.dll");
    shobs.add_symbol("SHStrDupW",  0x116D);
    reg.register(shobs);

    // ── api-ms-win-shcore-scaling-l1-1-1.dll ──
    let mut shscale = FakeDll::new("api-ms-win-shcore-scaling-l1-1-1.dll");
    shscale.add_symbol("GetDpiForMonitor",  0x207E);
    reg.register(shscale);

    // ── api-ms-win-eventing-provider-l1-1-0.dll ──
    let mut eventing = FakeDll::new("api-ms-win-eventing-provider-l1-1-0.dll");
    eventing.add_symbol("EventProviderEnabled",  0x116E);
    reg.register(eventing);

    // ── api-ms-win-core-shlwapi-legacy-l1-1-0.dll ──
    let mut shlwapi = FakeDll::new("api-ms-win-core-shlwapi-legacy-l1-1-0.dll");
    shlwapi.add_symbol("PathIsFileSpecW",    0x116F);
    shlwapi.add_symbol("PathFindExtensionW", 0x1170);
    reg.register(shlwapi);

    // ── COMCTL32.dll ──
    let mut comctl = FakeDll::new("comctl32.dll");
    comctl.add_symbol("CreateStatusWindowW", 0x2171);
    comctl.add_symbol("Ordinal#345",         0x2172);
    comctl.add_symbol("InitCommonControlsEx",0x2173);
    reg.register(comctl);

}
