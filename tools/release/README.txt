VEILOS
======

An operating system that runs in QEMU, a free virtual machine. You don't need
Rust or anything else; this folder has everything except QEMU itself.

1. Install QEMU (once)
   Windows  https://www.qemu.org/download/#windows  (64-bit installer, default options)
   macOS    brew install qemu
   Linux    sudo apt install qemu-system-x86   (or your distro's QEMU package)

2. Start VEILOS
   Windows  double-click run-windows.bat
   macOS / Linux  open a terminal in this folder and type:  sh run.sh

3. Log in
   admin   password: veilos
   root    no password

The first boot can take a minute or two; VEILOS runs emulated, so it's slower
than it would be on real hardware. VirtualBox and VMware aren't supported yet:
VEILOS only has a driver for QEMU's display.

Files you make inside VEILOS are saved in veilos-storage.img. To start over,
unzip a fresh copy. If something goes wrong, serial.log in this folder has the
boot messages; send it along with your report.

Built from commit: see VERSION.txt
