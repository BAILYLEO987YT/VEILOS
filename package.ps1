# VEILOS STANDALONE PACKAGER (Windows PowerShell)

Write-Host "--- VEILOS Packaging Script ---" -ForegroundColor Cyan

# 1. Compile the Kernel in RELEASE mode for maximum performance
Write-Host "[BUILD] Compiling VEILOS in Release Mode..." -ForegroundColor Yellow
cargo build --release

# 2. Check for the disk image
if (Test-Path "fat32.img") {
    Write-Host "[DISK] Found fat32.img, ready for bundling." -ForegroundColor Green
} else {
    Write-Host "[DISK] WARNING: fat32.img missing. Generating stub..." -ForegroundColor Red
    # Create a 64MB empty image File
    $f = New-Item -Path "fat32.img" -ItemType "file" -Value ""
    $f.SetLength(64MB)
}

# 3. Create a Portable Folder
$dist = "dist"
if (Test-Path $dist) { Remove-Item $dist -Recurse }
New-Item -ItemType Directory -Path $dist

# 4. Gather Artifacts
Copy-Item "target\x86_64-veilos\release\veilos" -Destination "$dist\veilos.efi" -ErrorAction SilentlyContinue
Copy-Item "fat32.img" -Destination "$dist\veilos_storage.img"
Copy-Item "TODO.md" -Destination "$dist\README.md"

Write-Host "--- Packaging Complete! ---" -ForegroundColor Green
Write-Host "Standalone binary located at: $dist\veilos.efi"
Write-Host "Storage image located at: $dist\veilos_storage.img"
