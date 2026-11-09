# VEILOS Dual-VM Launcher (Windows PowerShell)
# Launches two QEMU instances connected via socket netdev for mesh testing.
#
# Usage:
#   .\run_dual.ps1          # Build + launch both VMs
#   .\run_dual.ps1 -Build   # Force rebuild before launch
#   .\run_dual.ps1 -NoBuild # Skip build, just launch

param(
    [switch]$Build,
    [switch]$NoBuild
)

Write-Host "=== VEILOS Dual-VM Launcher ===" -ForegroundColor Cyan

# Determine build mode
$buildMode = "debug"
$targetDir = "target\x86_64-unknown-none\debug"
$bootImage = "$targetDir\bootimage-VEIL-OS.bin"

if ($Build -or (-not $NoBuild -and -not (Test-Path $bootImage))) {
    Write-Host "[BUILD] Building VEILOS..." -ForegroundColor Yellow
    cargo build 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[ERROR] Build failed!" -ForegroundColor Red
        exit 1
    }
    Write-Host "[BUILD] Build complete." -ForegroundColor Green
}
else {
    Write-Host "[BUILD] Skipping build (use -Build to force)." -ForegroundColor Gray
}

if (-not (Test-Path $bootImage)) {
    Write-Host "[ERROR] Boot image not found at $bootImage" -ForegroundColor Red
    Write-Host "        Run 'cargo build' first, or use -Build flag." -ForegroundColor Yellow
    exit 1
}

# Common QEMU arguments (no serial here — added per-VM)
$commonArgs = @(
    "-drive", "format=raw,file=$bootImage",
    "-cpu", "qemu64",
    "-smp", "4",
    "-m", "512M",
    "-accel", "whpx,kernel-irqchip=off",
    "-accel", "tcg,thread=single",
    "-device", "usb-ehci,id=usb",
    "-device", "usb-tablet,bus=usb.0",
    "-display", "sdl,gl=on",
    "-drive", "id=disk,file=fat:rw:storage,if=none,format=raw",
    "-device", "ahci,id=ahci",
    "-device", "ide-hd,drive=disk,bus=ahci.0"
)

# VM1: listens on 127.0.0.1:1234, sends to 127.0.0.1:1235
# Explicit MAC ending in 0x58 — previously this was left unset, and QEMU's
# implicit default for the first NIC (52:54:00:12:34:56) happened to be the
# exact same literal the kernel used as its placeholder NodeId, which is
# not a coincidence you want to rely on. Always set both MACs explicitly.
$vm1Args = $commonArgs + @(
    "-serial", "file:vm1_serial.log",
    "-netdev", "socket,id=net0,udp=127.0.0.1:1235,localaddr=127.0.0.1:1234",
    "-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:58"
)

# VM2: listens on 127.0.0.1:1235, sends to 127.0.0.1:1234
# Explicit MAC ending in 0x59 so the code can identify it
$vm2Args = $commonArgs + @(
    "-serial", "file:vm2_serial.log",
    "-netdev", "socket,id=net0,udp=127.0.0.1:1234,localaddr=127.0.0.1:1235",
    "-device", "virtio-net-pci,netdev=net0,mac=52:54:00:12:34:59"
)

Write-Host "[LAUNCH] Starting VM2..." -ForegroundColor Green
Write-Host "[LAUNCH] Starting VM1..." -ForegroundColor Green
Write-Host ""
Write-Host "VM1 netdev: udp=127.0.0.1:1235,localaddr=127.0.0.1:1234" -ForegroundColor Gray
Write-Host "VM1 MAC:    52:54:00:12:34:58" -ForegroundColor Gray
Write-Host "VM2 netdev: udp=127.0.0.1:1234,localaddr=127.0.0.1:1235" -ForegroundColor Gray
Write-Host "VM2 MAC:    52:54:00:12:34:59" -ForegroundColor Gray
Write-Host ""
Write-Host "Serial logs: vm1_serial.log, vm2_serial.log" -ForegroundColor Gray
Write-Host "Watch with:  Get-Content -Wait vm1_serial.log" -ForegroundColor Gray
Write-Host "             Get-Content -Wait vm2_serial.log (in another terminal)" -ForegroundColor Gray
Write-Host ""

# Launch VM2 in a new window (separate console so both run independently)
$vm2Process = Start-Process -FilePath "qemu-system-x86_64" `
    -ArgumentList $vm2Args `
    -WindowStyle Normal `
    -PassThru

# Small delay so VM2 can bind its socket before VM1 starts
Start-Sleep -Milliseconds 500

# Launch VM1 in a new window too (so both are independent)
$vm1Process = Start-Process -FilePath "qemu-system-x86_64" `
    -ArgumentList $vm1Args `
    -WindowStyle Normal `
    -PassThru

Write-Host "[RUNNING] Both VMs are running." -ForegroundColor Cyan
Write-Host "[RUNNING] Close the QEMU windows to stop, or press any key here to kill both." -ForegroundColor Yellow
Write-Host ""

# Wait for user input, then clean up
$null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")

# Clean up
if (-not $vm1Process.HasExited) {
    Write-Host "[CLEANUP] Stopping VM1..." -ForegroundColor Yellow
    $vm1Process.Kill()
}
if (-not $vm2Process.HasExited) {
    Write-Host "[CLEANUP] Stopping VM2..." -ForegroundColor Yellow
    $vm2Process.Kill()
}

Write-Host "[DONE] Both VMs stopped." -ForegroundColor Cyan