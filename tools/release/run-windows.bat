@echo off
rem Start VEILOS in QEMU. Double-click this file.
rem Needs QEMU for Windows: https://www.qemu.org/download/#windows
setlocal
cd /d "%~dp0"

set "QEMU=%ProgramFiles%\qemu\qemu-system-x86_64.exe"
if exist "%QEMU%" goto run
set "QEMU="
for %%Q in (qemu-system-x86_64.exe) do set "QEMU=%%~$PATH:Q"
if defined QEMU goto run

echo QEMU isn't installed.
echo Download the 64-bit Windows installer from https://www.qemu.org/download/#windows
echo install it with the default options, then run this file again.
pause
exit /b 1

:run
echo Starting VEILOS. The first boot can take a minute or two.
echo Log in as "admin" with password "veilos" (or "root" with no password).
"%QEMU%" -cpu qemu64 -smp 4 -m 1G -accel tcg,thread=multi ^
  -drive format=raw,file=veilos-boot.img ^
  -vga none -device bochs-display ^
  -device usb-ehci,id=usb -device usb-tablet,bus=usb.0 ^
  -drive id=disk,file=veilos-storage.img,if=none,format=raw ^
  -device ahci,id=ahci -device ide-hd,drive=disk,bus=ahci.0 ^
  -netdev user,id=net0 -device virtio-net-pci,netdev=net0 ^
  -serial file:serial.log %*
if errorlevel 1 pause
