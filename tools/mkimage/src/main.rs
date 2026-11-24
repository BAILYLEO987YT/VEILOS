//! mkimage KERNEL OUT_DIR [--bios] [--uefi] [--ramdisk IMAGE]
//!
//! Boot disk images for the kernel ELF, with bootloader 0.11:
//!
//!   OUT_DIR/veilos-bios.img  MBR, BIOS boot (QEMU's SeaBIOS, PCs with CSM)
//!   OUT_DIR/veilos-uefi.img  GPT with an EFI System Partition (UEFI PCs, OVMF)
//!
//! Both by default. The firmware loads the kernel from them. VEILOS's own
//! volume (the one with VEILOS.VOL) is added as a partition after them by
//! `tools/mkdisk.py --boot` (one disk), or is a separate disk.
//!
//! --ramdisk IMAGE puts a disk image (tools/mkdisk.py) beside the kernel:
//! the firmware loads it into memory with the kernel and VEILOS runs from it
//! (src/fs/block.rs), for machines where VEILOS can't reach its disk yet (a USB
//! stick). Those images are named veilos-bios-ram.img and veilos-uefi-ram.img.
//!
//! Run through tools/build.sh, which builds the kernel first and runs this
//! from outside the repo (the repo's .cargo/config.toml is for the bare-metal
//! kernel, not for a host program).

use std::path::PathBuf;
use std::process::exit;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut paths = Vec::new();
    let (mut bios, mut uefi, mut ramdisk) = (false, false, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bios" => bios = true,
            "--uefi" => uefi = true,
            "--ramdisk" => ramdisk = Some(PathBuf::from(args.next().unwrap_or_else(|| usage()))),
            s if s.starts_with("--") => usage(),
            _ => paths.push(PathBuf::from(a)),
        }
    }
    if paths.len() != 2 {
        usage();
    }
    if !bios && !uefi {
        (bios, uefi) = (true, true);
    }
    let (kernel, out) = (paths.remove(0), paths.remove(0));
    if !kernel.is_file() {
        eprintln!("mkimage: no kernel at {}", kernel.display());
        exit(1);
    }
    std::fs::create_dir_all(&out).expect("can't make the output folder");

    let mut builder = bootloader::DiskImageBuilder::new(kernel);
    let suffix = match &ramdisk {
        Some(r) if !r.is_file() => {
            eprintln!("mkimage: no RAM disk image at {}", r.display());
            exit(1);
        }
        Some(r) => {
            builder.set_ramdisk(r.clone());
            "-ram"
        }
        None => "",
    };
    if bios {
        let path = out.join(format!("veilos-bios{}.img", suffix));
        builder.create_bios_image(&path).unwrap_or_else(|e| fail("BIOS", e));
        println!("{}", path.display());
    }
    if uefi {
        let path = out.join(format!("veilos-uefi{}.img", suffix));
        builder.create_uefi_image(&path).unwrap_or_else(|e| fail("UEFI", e));
        println!("{}", path.display());
    }
}

fn usage() -> ! {
    eprintln!("usage: mkimage KERNEL OUT_DIR [--bios] [--uefi] [--ramdisk IMAGE]");
    exit(2);
}

fn fail(what: &str, e: anyhow::Error) -> ! {
    eprintln!("mkimage: the {} image failed: {:#}", what, e);
    exit(1);
}
