# run_claude.ps1 -- VEILOS in a window, on the disk that has claude.exe and Git.
#
# The disk is %LOCALAPPDATA%\VEILOS\claude-disk.img: those programs and the
# sign-in stay out of the repository (and out of OneDrive). What changes on it
# persists. The kernel is this repository's latest `cargo bootimage`.
#
#   powershell -ExecutionPolicy Bypass -File tools\run_claude.ps1
#
# Under WHPX the BIOS sometimes hangs before the kernel starts, so the window
# can close and reopen a number of times; that is this script starting again.
param([int]$Cpus = 4, [string]$Ram = '4G')

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$boot = Join-Path $repo 'target\x86_64-unknown-none\debug\bootimage-VEIL-OS.bin'
$dir  = Join-Path $env:LOCALAPPDATA 'VEILOS'
$disk = Join-Path $dir 'claude-disk.img'
$log  = Join-Path $dir 'serial.log'
$qemu = 'C:\Program Files\qemu\qemu-system-x86_64.exe'

if (-not (Test-Path $boot)) { throw "No kernel at $boot. Run: cargo bootimage" }
if (-not (Test-Path $disk)) { throw "No disk at $disk." }
if (-not (Test-Path $qemu)) { throw "QEMU isn't at $qemu." }

$qargs = @(
    '-machine', 'q35', '-accel', 'whpx', '-cpu', 'qemu64', '-smp', "$Cpus", '-m', $Ram,
    '-drive', "format=raw,file=`"$boot`",snapshot=on",
    '-serial', "file:`"$log`"",
    '-display', 'sdl',
    '-monitor', 'tcp:127.0.0.1:4566,server,nowait',
    '-vga', 'none', '-device', 'bochs-display',
    '-drive', "id=disk,file=`"$disk`",if=none,format=raw",
    '-device', 'ide-hd,drive=disk,bus=ide.1',
    '-netdev', 'user,id=net0', '-device', 'virtio-net-pci,netdev=net0'
)

# The size QEMU has written so far (the directory entry lags behind an open file).
function Get-LogLength {
    try {
        $s = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite,Delete')
        try { return $s.Length } finally { $s.Dispose() }
    } catch { return 0 }
}

foreach ($try in 1..60) {
    Remove-Item $log -ErrorAction SilentlyContinue
    $p = Start-Process -FilePath $qemu -ArgumentList $qargs -PassThru
    Start-Sleep -Seconds 8
    if ($p.HasExited) { throw 'QEMU stopped at once. Is another VEILOS VM still running?' }
    if ((Get-LogLength) -gt 0) {
        Write-Host "Booted (try $try). Log in, open MENU > Terminal, then type:"
        Write-Host ''
        Write-Host '    run /cmd.exe /c \claude.cmd'
        Write-Host ''
        Write-Host 'claude.exe takes about two minutes to start. Click only inside the Terminal.'
        return
    }
    Stop-Process -Id $p.Id -Force
    Start-Sleep -Seconds 2
}
throw 'The BIOS hung on all 60 tries.'
