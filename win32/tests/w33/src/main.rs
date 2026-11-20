// AVX state across thread switches: each thread keeps its own values in
// YMM1-YMM5 (all 256 bits) while it spins, then checks them.
use std::arch::asm;
use std::arch::x86_64::{__cpuid, __cpuid_count};

#[target_feature(enable = "avx2")]
unsafe fn round(seed: u64, spin: u64) -> u32 {
    let bad: u32;
    asm!(
        "mov rax, {s}", "vmovq xmm0, rax", "vpbroadcastq ymm1, xmm0",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm2, xmm0",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm3, xmm0",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm4, xmm0",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm5, xmm0",
        "mov rcx, {n}",
        "2: dec rcx", "jnz 2b",
        "xor edx, edx",
        "mov rax, {s}", "vmovq xmm0, rax", "vpbroadcastq ymm0, xmm0",
        "vpcmpeqq ymm0, ymm0, ymm1", "vmovmskpd ecx, ymm0", "cmp ecx, 15", "setne cl", "movzx ecx, cl", "add edx, ecx",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm0, xmm0",
        "vpcmpeqq ymm0, ymm0, ymm2", "vmovmskpd ecx, ymm0", "cmp ecx, 15", "setne cl", "movzx ecx, cl", "add edx, ecx",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm0, xmm0",
        "vpcmpeqq ymm0, ymm0, ymm3", "vmovmskpd ecx, ymm0", "cmp ecx, 15", "setne cl", "movzx ecx, cl", "add edx, ecx",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm0, xmm0",
        "vpcmpeqq ymm0, ymm0, ymm4", "vmovmskpd ecx, ymm0", "cmp ecx, 15", "setne cl", "movzx ecx, cl", "add edx, ecx",
        "inc rax", "vmovq xmm0, rax", "vpbroadcastq ymm0, xmm0",
        "vpcmpeqq ymm0, ymm0, ymm5", "vmovmskpd ecx, ymm0", "cmp ecx, 15", "setne cl", "movzx ecx, cl", "add edx, ecx",
        "vzeroupper",
        s = in(reg) seed, n = in(reg) spin,
        out("rax") _, out("rcx") _, out("edx") bad,
        out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _, out("xmm5") _,
    );
    bad
}

fn main() {
    let c1 = unsafe { __cpuid(1) };
    let c7 = unsafe { __cpuid_count(7, 0) };
    let avx = c1.ecx & (1 << 28) != 0;
    let osxsave = c1.ecx & (1 << 27) != 0;
    let avx2 = c7.ebx & (1 << 5) != 0;
    let xcr0 = if osxsave {
        let (lo, hi): (u32, u32);
        unsafe { asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi) };
        (hi as u64) << 32 | lo as u64
    } else { 0 };
    println!("avx1: CPUID AVX {} AVX2 {} OSXSAVE {}; XCR0 {:#x}", avx, avx2, osxsave, xcr0);
    if !(avx && avx2 && xcr0 & 6 == 6) {
        println!("avx1: AVX not enabled by the OS; nothing to test");
        std::process::exit(100);
    }
    let rounds: u64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(40);
    let t0 = std::time::Instant::now();
    let threads: Vec<_> = (0..4u64)
        .map(|t| std::thread::spawn(move || {
            let mut bad = 0;
            for r in 0..rounds {
                bad += unsafe { round(0x1111_0000_0000_0000 * (t + 1) + r * 16, 20_000_000) };
            }
            bad
        }))
        .collect();
    let bad: u32 = threads.into_iter().map(|h| h.join().unwrap()).sum();
    println!("avx1: 4 threads x {} rounds in {:?}: {} bad registers", rounds, t0.elapsed(), bad);
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
