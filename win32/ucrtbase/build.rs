// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // compiler_builtins' memory functions, exported as the C runtime's.
    for f in ["memcpy", "memmove", "memset", "memcmp"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}");
    }
}
