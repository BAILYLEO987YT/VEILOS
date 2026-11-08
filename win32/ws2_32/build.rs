// No C runtime: our own entry point, nothing linked by default. Winsock's
// functions keep Windows' ordinals: MSVC's ws2_32.lib imports most by number.
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-cdylib-link-arg=/ENTRY:DllMain");
    // Its own preferred base (win32/build.sh lists them all): the same
    // address in every process, as Windows' system DLLs are.
    println!("cargo:rustc-cdylib-link-arg=/BASE:0x7e0009000000");
    let ordinals = [
        ("accept", 1), ("bind", 2), ("closesocket", 3), ("connect", 4), ("getpeername", 5), ("getsockname", 6),
        ("getsockopt", 7), ("htonl", 8), ("htons", 9), ("ioctlsocket", 10), ("inet_addr", 11), ("inet_ntoa", 12),
        ("listen", 13), ("ntohl", 14), ("ntohs", 15), ("recv", 16), ("recvfrom", 17), ("select", 18), ("send", 19),
        ("sendto", 20), ("setsockopt", 21), ("shutdown", 22), ("socket", 23), ("gethostbyaddr", 51),
        ("gethostbyname", 52), ("getprotobyname", 53), ("getprotobynumber", 54), ("getservbyname", 55),
        ("getservbyport", 56), ("gethostname", 57), ("WSAGetLastError", 111), ("WSASetLastError", 112),
        ("WSAStartup", 115), ("WSACleanup", 116), ("__WSAFDIsSet", 151),
    ];
    for (name, n) in ordinals {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{name},@{n}");
    }
    // Not offered yet: sharing a socket with another process, overlapped results.
    println!("cargo:rustc-cdylib-link-arg=/EXPORT:WSADuplicateSocketW=kernel32.VeilSocketUnsupported");
    println!("cargo:rustc-cdylib-link-arg=/EXPORT:WSAGetOverlappedResult=kernel32.VeilCryptUnsupported");
}
