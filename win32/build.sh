#!/bin/sh
# Build VEILOS's Windows system DLLs and install them into storage/system.
# Run from anywhere: sh win32/build.sh
set -e
# Git Bash would rewrite /flags as paths.
export MSYS_NO_PATHCONV=1
cd "$(dirname "$0")"
LLVM="/c/Program Files/LLVM/bin"
OUT=target/x86_64-pc-windows-msvc/release
cargo build --release
mkdir -p ../storage/system
for dll in ntdll kernel32 ucrtbase gdi32 user32 comctl32 shell32 comdlg32 riched20; do
    cp "$OUT/$dll.dll" "../storage/system/$dll.dll"
done

# msftedit.dll (and the older riched32.dll): the same rich edit DLL.
cp "$OUT/riched20.dll" ../storage/system/msftedit.dll
cp "$OUT/riched20.dll" ../storage/system/riched32.dll

# msvcrt.dll and vcruntime140(_1).dll: forwarders to ucrtbase.dll, made
# from its export list (no code, no entry point).
mkdir -p target/forwarders
python crt_forwarders.py "$(cygpath -w "$LLVM/llvm-objdump.exe")" "$OUT/ucrtbase.dll" target/forwarders
echo "" > target/forwarders/empty.c
"$LLVM/clang-cl.exe" /nologo /c target/forwarders/empty.c /Fotarget/forwarders/empty.obj
for dll in msvcrt vcruntime140 vcruntime140_1; do
    "$LLVM/lld-link.exe" /nologo /DLL /NOENTRY /NODEFAULTLIB /MACHINE:X64 \
        /DEF:target/forwarders/$dll.def target/forwarders/empty.obj /OUT:target/forwarders/$dll.dll
    cp target/forwarders/$dll.dll "../storage/system/$dll.dll"
done

# msimg32.dll: AlphaBlend and TransparentBlt live in gdi32, as on Windows.
printf 'LIBRARY msimg32.dll\nEXPORTS\n    AlphaBlend=gdi32.GdiAlphaBlend\n    TransparentBlt=gdi32.GdiTransparentBlt\n' > target/forwarders/msimg32.def
"$LLVM/lld-link.exe" /nologo /DLL /NOENTRY /NODEFAULTLIB /MACHINE:X64 \
    /DEF:target/forwarders/msimg32.def target/forwarders/empty.obj /OUT:target/forwarders/msimg32.dll
cp target/forwarders/msimg32.dll ../storage/system/msimg32.dll

# urlmon.dll, propsys.dll, winspool.drv: the few functions programs reach
# for, kept in kernel32.
fwd() {
    out=$1; shift
    { printf 'LIBRARY %s
EXPORTS
' "$out"; for f in "$@"; do printf '    %s=kernel32.%s
' "$f" "$f"; done; } > "target/forwarders/$out.def"
    "$LLVM/lld-link.exe" /nologo /DLL /NOENTRY /NODEFAULTLIB /MACHINE:X64         /DEF:"target/forwarders/$out.def" target/forwarders/empty.obj /OUT:"target/forwarders/$out"
    cp "target/forwarders/$out" "../storage/system/$out"
}
# advapi32.dll: the registry, tokens, ETW and the rest kernel32 implements.
fwd advapi32.dll CreateProcessAsUserW DecryptFileW DuplicateEncryptionInfoFile GetFileSecurityW     GetSecurityDescriptorOwner GetTokenInformation GetUserNameA GetUserNameW IsTextUnicode OpenProcessToken     OpenThreadToken RevertToSelf AdjustTokenPrivileges LookupPrivilegeValueW LookupAccountSidW InitializeAcl \
    InitializeSecurityDescriptor IsValidSecurityDescriptor GetSecurityDescriptorControl GetSecurityDescriptorDacl \
    GetSecurityDescriptorGroup GetSecurityDescriptorSacl SetSecurityDescriptorDacl SetSecurityDescriptorGroup \
    SetSecurityDescriptorOwner SetSecurityDescriptorSacl GetSidSubAuthority GetSidSubAuthorityCount MapGenericMask \
    GetNamedSecurityInfoW SetNamedSecurityInfoW GetSecurityInfo SetSecurityInfo RegFlushKey RegSetValueExA \
    RegSetValueW RegDeleteKeyW RegEnumKeyW RegRenameKey RegConnectRegistryW RegLoadKeyW RegUnLoadKeyW RegSaveKeyW \
    RegOpenKeyA RegOpenCurrentUser RegDeleteKeyA RegDeleteKeyExA RegDeleteTreeW RegDeleteTreeA RegGetValueA RegQueryValueW \
    RegDeleteValueA RegEnumKeyExA RegEnumValueA RegQueryInfoKeyA \
    RegRestoreKeyW RegSetKeySecurity RegNotifyChangeKeyValue RegCloseKey RegCreateKeyA RegCreateKeyExA RegCreateKeyExW RegCreateKeyW     RegDeleteKeyExW RegDeleteValueW RegEnumKeyExW RegEnumValueW RegGetValueW RegOpenKeyExA RegOpenKeyExW     RegOpenKeyW RegQueryInfoKeyW RegQueryValueExA RegQueryValueExW RegSetKeyValueA RegSetKeyValueW     RegSetValueExW EventActivityIdControl EventEnabled EventProviderEnabled EventRegister     EventSetInformation EventUnregister EventWrite EventWriteEx EventWriteTransfer
fwd urlmon.dll FindMimeFromData
fwd authz.dll AuthzInitializeResourceManager AuthzFreeResourceManager AuthzInitializeContextFromSid AuthzFreeContext AuthzAccessCheck
fwd version.dll GetFileVersionInfoSizeW GetFileVersionInfoSizeA GetFileVersionInfoW GetFileVersionInfoA VerQueryValueW VerQueryValueA
fwd propsys.dll PSGetPropertyDescriptionListFromString PropVariantToStringVectorAlloc
fwd winspool.drv OpenPrinterW OpenPrinterA ClosePrinter GetPrinterDriverW EnumPrintersW GetDefaultPrinterW DocumentPropertiesW
echo "installed: $(ls ../storage/system | tr '\n' ' ')"
