// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // Its own preferred base (win32/build.sh lists them all): the same
    // address in every process, as Windows' system DLLs are.
    println!("cargo:rustc-cdylib-link-arg=/BASE:0x7e0001000000");
    // kernel32 exports these, but they are ntdll's.
    for f in [
        "RtlCaptureContext", "RtlCaptureStackBackTrace", "RtlLookupFunctionEntry", "RtlPcToFileHeader",
        "RtlRaiseException", "RtlRestoreContext", "RtlUnwind", "RtlUnwindEx", "RtlVirtualUnwind",
        "RtlAddFunctionTable", "RtlDeleteFunctionTable",
    ] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}=ntdll.{f}");
    }
}
