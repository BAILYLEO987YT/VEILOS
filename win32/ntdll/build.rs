// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // Written in global_asm! (they carry their own unwind info).
    for f in ["KiUserExceptionDispatcher", "RtlRestoreContext"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}");
    }
}
