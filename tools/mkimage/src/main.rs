//! mkimage KERNEL OUT_DIR [--bios] [--uefi]
//!
//! Boot disk images for the kernel ELF, with bootloader 0.11:
//!
//!   OUT_DIR/veilos-bios.img  MBR, BIOS boot (QEMU's SeaBIOS, PCs with CSM)
//!   OUT_DIR/veilos-uefi.img  GPT with an EFI System Partition (UEFI PCs, OVMF)
//!
//! Both by default. The firmware loads the kernel from them; VEILOS's own
//! volume (the one with VEILOS.VOL) is a separate disk for now, made by
//! tools/mkdisk.py. Run through tools/build.sh, which builds the kernel first
//! and runs this from outside the repo (the repo's .cargo/config.toml is for
//! the bare-metal kernel, not for a host program).

use std::path::PathBuf;
use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: Vec<&str> = args.iter().filter(|a| a.starts_with("--")).map(|s| s.as_str()).collect();
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.len() != 2 || flags.iter().any(|f| !matches!(*f, "--bios" | "--uefi")) {
        eprintln!("usage: mkimage KERNEL OUT_DIR [--bios] [--uefi]");
        exit(2);
    }
    let kernel = PathBuf::from(paths[0]);
    let out = PathBuf::from(paths[1]);
    let both = flags.is_empty();
    if !kernel.is_file() {
        eprintln!("mkimage: no kernel at {}", kernel.display());
        exit(1);
    }
    std::fs::create_dir_all(&out).expect("can't make the output folder");

    let builder = bootloader::DiskImageBuilder::new(kernel);
    if both || flags.contains(&"--bios") {
        let path = out.join("veilos-bios.img");
        builder.create_bios_image(&path).unwrap_or_else(|e| fail("BIOS", e));
        println!("{}", path.display());
    }
    if both || flags.contains(&"--uefi") {
        let path = out.join("veilos-uefi.img");
        builder.create_uefi_image(&path).unwrap_or_else(|e| fail("UEFI", e));
        println!("{}", path.display());
    }
}

fn fail(what: &str, e: anyhow::Error) -> ! {
    eprintln!("mkimage: the {} image failed: {:#}", what, e);
    exit(1);
}
