// No C runtime: our own entry point, nothing linked by default. Some
// exports are known by ordinal (programs import them by number), as
// Windows' comctl32 numbers them.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    for (name, ord) in [
        ("MenuHelp", 2),
        ("GetEffectiveClientRect", 4),
        ("InitCommonControls", 17),
        ("Str_SetPtrW", 236),
        ("DSA_Create", 320),
        ("DSA_Destroy", 321),
        ("DSA_GetItem", 322),
        ("DSA_GetItemPtr", 323),
        ("DSA_InsertItem", 324),
        ("DSA_SetItem", 325),
        ("DSA_DeleteItem", 326),
        ("DSA_DeleteAllItems", 327),
        ("DPA_Create", 328),
        ("DPA_Destroy", 329),
        ("DPA_Grow", 330),
        ("DPA_Clone", 331),
        ("DPA_GetPtr", 332),
        ("DPA_GetPtrIndex", 333),
        ("DPA_InsertPtr", 334),
        ("DPA_SetPtr", 335),
        ("DPA_DeletePtr", 336),
        ("DPA_DeleteAllPtrs", 337),
        ("DPA_Sort", 338),
        ("DPA_Search", 339),
        ("DPA_CreateEx", 340),
        ("TaskDialog", 344),
        ("TaskDialogIndirect", 345),
        ("DPA_EnumCallback", 385),
        ("DPA_DestroyCallback", 386),
        ("DSA_EnumCallback", 387),
        ("DSA_DestroyCallback", 388),
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
