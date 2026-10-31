// No C runtime: our own entry point, nothing linked by default.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    for f in ["LoadStringW", "LoadStringA"] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{f}=kernel32.{f}");
    }
}
