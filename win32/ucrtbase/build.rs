// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // Its own preferred base (win32/build.sh lists them all): the same
    // address in every process, as Windows' system DLLs are.
    println!("cargo:rustc-cdylib-link-arg=/BASE:0x7e0002000000");
    // compiler_builtins' memory functions, exported as the C runtime's.
    for f in ["memcpy", "memmove", "memset", "memcmp"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}");
    }
}
