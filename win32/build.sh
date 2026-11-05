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
for dll in ntdll kernel32 ucrtbase gdi32 user32 comctl32 shell32 comdlg32 riched20 ws2_32; do
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
fwd advapi32.dll CryptImportKey CryptEncrypt CryptDestroyKey IsValidSid GetLengthSid EqualSid CopySid AllocateAndInitializeSid FreeSid IsWellKnownSid CheckTokenMembership ConvertSidToStringSidA ConvertSidToStringSidW SystemFunction036 CryptAcquireContextA CryptAcquireContextW CryptReleaseContext CryptGenRandom \
    CryptCreateHash CryptHashData CryptGetHashParam CryptDestroyHash CreateProcessAsUserW DecryptFileW DuplicateEncryptionInfoFile GetFileSecurityW     GetSecurityDescriptorOwner GetTokenInformation GetUserNameA GetUserNameW IsTextUnicode OpenProcessToken     OpenThreadToken RevertToSelf AdjustTokenPrivileges LookupPrivilegeValueW LookupAccountSidW InitializeAcl \
    InitializeSecurityDescriptor IsValidSecurityDescriptor GetSecurityDescriptorControl GetSecurityDescriptorDacl \
    GetSecurityDescriptorGroup GetSecurityDescriptorSacl SetSecurityDescriptorDacl SetSecurityDescriptorGroup \
    SetSecurityDescriptorOwner SetSecurityDescriptorSacl GetSidSubAuthority GetSidSubAuthorityCount MapGenericMask \
    GetNamedSecurityInfoW SetNamedSecurityInfoW GetSecurityInfo SetSecurityInfo RegFlushKey RegSetValueExA \
    RegSetValueW RegDeleteKeyW RegEnumKeyW RegRenameKey RegConnectRegistryW RegLoadKeyW RegUnLoadKeyW RegSaveKeyW \
    RegOpenKeyA RegOpenCurrentUser RegDeleteKeyA RegDeleteKeyExA RegDeleteTreeW RegDeleteTreeA RegGetValueA RegQueryValueW \
    RegDeleteValueA RegEnumKeyExA RegEnumValueA RegQueryInfoKeyA \
    RegRestoreKeyW RegSetKeySecurity RegNotifyChangeKeyValue RegCloseKey RegCreateKeyA RegCreateKeyExA RegCreateKeyExW RegCreateKeyW     RegDeleteKeyExW RegDeleteValueW RegEnumKeyExW RegEnumValueW RegGetValueW RegOpenKeyExA RegOpenKeyExW     RegOpenKeyW RegQueryInfoKeyW RegQueryValueExA RegQueryValueExW RegSetKeyValueA RegSetKeyValueW     RegSetValueExW EventActivityIdControl EventEnabled EventProviderEnabled EventRegister     EventSetInformation EventUnregister EventWrite EventWriteEx EventWriteTransfer
fwd urlmon.dll FindMimeFromData
# What the Rust toolchain loads besides.
fwd bcryptprimitives.dll ProcessPrng
fwd bcrypt.dll BCryptGenRandom
# A forwarder DLL from its .def: what it doesn't offer yet points at kernel32
# functions that fail plainly (VeilCngUnsupported and friends, toolchain.rs).
fwd_link() {
    "$LLVM/lld-link.exe" /nologo /DLL /NOENTRY /NODEFAULTLIB /MACHINE:X64 /DEF:"target/forwarders/$1.def" target/forwarders/empty.obj /OUT:"target/forwarders/$1"
    cp "target/forwarders/$1" "../storage/system/$1"
}
# bcrypt.dll: random numbers; no algorithms yet.
printf 'LIBRARY bcrypt.dll\nEXPORTS\n    BCryptGenRandom=kernel32.BCryptGenRandom\n' > target/forwarders/bcrypt.dll.def
for f in BCryptCloseAlgorithmProvider BCryptCreateHash BCryptDecrypt BCryptDeriveKey BCryptDestroyHash BCryptDestroyKey BCryptDestroySecret BCryptEncrypt BCryptExportKey BCryptFinalizeKeyPair BCryptFinishHash BCryptGenerateKeyPair BCryptGetProperty BCryptHashData BCryptImportKey BCryptImportKeyPair BCryptOpenAlgorithmProvider BCryptSecretAgreement BCryptSetProperty BCryptSignHash BCryptVerifySignature BCryptGenerateSymmetricKey BCryptDuplicateHash BCryptHash BCryptKeyDerivation BCryptDeriveKeyPBKDF2; do printf '    %s=kernel32.VeilCngUnsupported\n' "$f" >> target/forwarders/bcrypt.dll.def; done
fwd_link bcrypt.dll
# crypt32.dll: the server certificates Schannel hands out (secur32 keeps them);
# no certificate stores of its own yet.
printf 'LIBRARY crypt32.dll\nEXPORTS\n' > target/forwarders/crypt32.dll.def
for f in CertCloseStore CertEnumCertificatesInStore CertFreeCertificateContext CertDuplicateCertificateContext; do printf '    %s=secur32.%s\n' "$f" "$f" >> target/forwarders/crypt32.dll.def; done
for f in CertAddCertificateContextToStore CertCreateCertificateChainEngine CertFindCertificateInStore CertFindExtension CertFreeCRLContext CertFreeCTLContext CertFreeCertificateChain CertFreeCertificateChainEngine CertGetCertificateChain CertGetNameStringA CertGetNameStringW CertOpenStore CertOpenSystemStoreA CertOpenSystemStoreW CryptDecodeObjectEx CryptQueryObject CryptStringToBinaryA CryptStringToBinaryW CryptBinaryToStringA CryptBinaryToStringW PFXImportCertStore CertCreateCertificateContext CertGetCertificateContextProperty CertVerifyCertificateChainPolicy CryptProtectData CryptUnprotectData; do printf '    %s=kernel32.VeilCryptUnsupported\n' "$f" >> target/forwarders/crypt32.dll.def; done
fwd_link crypt32.dll
# wldap32.dll: no LDAP.
printf 'LIBRARY wldap32.dll\nEXPORTS\n' > target/forwarders/wldap32.dll.def
for f in ldap_bind_s ldap_search_s ldap_set_option ldap_simple_bind_s ldap_unbind_s ldap_msgfree ldap_value_free_len ldap_memfree ber_free; do printf '    %s=kernel32.VeilLdapUnsupported\n' "$f" >> target/forwarders/wldap32.dll.def; done
for f in ldap_err2string ldap_first_attribute ldap_first_entry ldap_get_dn ldap_get_values_len ldap_next_attribute ldap_next_entry ldap_sslinit ldap_init; do printf '    %s=kernel32.VeilCryptUnsupported\n' "$f" >> target/forwarders/wldap32.dll.def; done
fwd_link wldap32.dll
# iphlpapi.dll: interface names have no numbers here (0 is "not found").
printf 'LIBRARY iphlpapi.dll\nEXPORTS\n    if_nametoindex=kernel32.VeilCryptUnsupported\n' > target/forwarders/iphlpapi.dll.def
fwd_link iphlpapi.dll
fwd userenv.dll GetUserProfileDirectoryW
fwd oleaut32.dll SysAllocStringLen SysFreeString SysStringLen GetErrorInfo SetErrorInfo
fwd ole32.dll CoCreateGuid CoCreateInstance CoInitializeEx CoUninitialize CoTaskMemFree CoCreateFreeThreadedMarshaler
fwd authz.dll AuthzInitializeResourceManager AuthzFreeResourceManager AuthzInitializeContextFromSid AuthzFreeContext AuthzAccessCheck
fwd version.dll GetFileVersionInfoSizeW GetFileVersionInfoSizeA GetFileVersionInfoW GetFileVersionInfoA VerQueryValueW VerQueryValueA
fwd propsys.dll PSGetPropertyDescriptionListFromString PropVariantToStringVectorAlloc
fwd winspool.drv OpenPrinterW OpenPrinterA ClosePrinter GetPrinterDriverW EnumPrintersW GetDefaultPrinterW DocumentPropertiesW
echo "installed: $(ls ../storage/system | tr '\n' ' ')"

# secur32.dll: Schannel over rustls. It uses the standard library (and crates
# from crates.io), so it's built on its own, outside this folder: the kernel's
# .cargo/config.toml (build-std) would apply to anything below the repository.
SECUR32_WORK="${TEMP:-/tmp}/veilos-secur32"
rm -rf "$SECUR32_WORK/src"
mkdir -p "$SECUR32_WORK"
cp -r secur32/Cargo.toml secur32/src "$SECUR32_WORK/"
(cd "$SECUR32_WORK" && cargo build --release --target x86_64-pc-windows-msvc)
cp "$SECUR32_WORK/target/x86_64-pc-windows-msvc/release/secur32.dll" ../storage/system/secur32.dll
