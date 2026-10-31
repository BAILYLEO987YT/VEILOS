// No C runtime: our own entry point, nothing linked by default. Some
// exports are known by ordinal (programs import them by number), as
// Windows' comctl32 numbers them.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    for (name, ord) in [
        ("InitCommonControls", 17),
        ("LoadIconMetric", 380),
        ("LoadIconWithScaleDown", 381),
        ("SetWindowSubclass", 410),
        ("GetWindowSubclass", 411),
        ("RemoveWindowSubclass", 412),
        ("DefSubclassProc", 413),
    ] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{name},@{ord}");
    }
}
