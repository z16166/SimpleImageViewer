# setup_libraw.ps1
# This script downloads and extracts LibRaw for local development.
# The 3rdparty/LibRaw directory is excluded from Git.

$Url = "https://www.libraw.org/data/LibRaw-0.22.1.zip"
$ZipFile = "libraw_tmp.zip"
$DestDir = "3rdparty/LibRaw"

Write-Host "--- Standardizing LibRaw dependency for local build ---" -ForegroundColor Cyan

if (Test-Path $DestDir) {
    Write-Host "Cleaning existing directory..."
    Remove-Item $DestDir -Recurse -Force
}

Write-Host "Downloading LibRaw 0.22.1 from official site..."
Invoke-WebRequest -Uri $Url -OutFile $ZipFile

Write-Host "Extracting..."
Expand-Archive -Path $ZipFile -DestinationPath "3rdparty" -Force

Write-Host "Organizing files..."
Move-Item "3rdparty/LibRaw-0.22.1" $DestDir -Force

Write-Host "Cleaning up..."
Remove-Item $ZipFile -Force

Write-Host "Done! You can now run 'cargo build'." -ForegroundColor Green
