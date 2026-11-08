// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // Its own preferred base (win32/build.sh lists them all): the same
    // address in every process, as Windows' system DLLs are.
    println!("cargo:rustc-cdylib-link-arg=/BASE:0x7e0004000000");
    for f in ["LoadStringW", "LoadStringA"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}=kernel32.{f}");
    }
    // Desktops and window stations aren't kept: these fail plainly
    // (FALSE or NULL, ERROR_NOT_SUPPORTED).
    for f in ["CloseDesktop", "CreateDesktopW", "GetProcessWindowStation", "GetThreadDesktop",
              "GetUserObjectInformationW", "GetUserObjectSecurity", "SetUserObjectSecurity"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}=kernel32.VeilCryptUnsupported");
    }
}
