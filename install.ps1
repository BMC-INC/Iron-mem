# install.ps1 — IronMem installer for Windows
# Usage: powershell -ExecutionPolicy Bypass -File install.ps1

$ErrorActionPreference = "Stop"

Write-Host ""
Write-Host "  Installing IronMem..." -ForegroundColor Cyan
Write-Host ""

# Check for Rust/Cargo
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Host "  ERROR: Rust/Cargo not found." -ForegroundColor Red
    Write-Host "  Install from: https://rustup.rs" -ForegroundColor Yellow
    exit 1
}

# Build release binary
Write-Host "  Building ironmem (release)..." -ForegroundColor Yellow
cargo build --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "  Build failed." -ForegroundColor Red
    exit 1
}

# Create install directory
$installDir = Join-Path $env:USERPROFILE ".ironmem\bin"
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
}

# Copy binary
$source = Join-Path $PSScriptRoot "target\release\ironmem.exe"
$dest = Join-Path $installDir "ironmem.exe"
Copy-Item $source $dest -Force
Write-Host "  Installed to: $dest" -ForegroundColor Green

# Add to PATH if not already there
$userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable("PATH", "$userPath;$installDir", "User")
    Write-Host "  Added $installDir to user PATH" -ForegroundColor Green
    Write-Host "  Restart your terminal for PATH changes to take effect." -ForegroundColor Yellow
}

# Check for API key
if (-not $env:ANTHROPIC_API_KEY) {
    Write-Host ""
    Write-Host "  NOTE: Set your API key:" -ForegroundColor Yellow
    Write-Host '  $env:ANTHROPIC_API_KEY = "your-key-here"' -ForegroundColor White
    Write-Host "  Or add it to your PowerShell profile for persistence." -ForegroundColor White
}

Write-Host ""
Write-Host "  IronMem installed successfully!" -ForegroundColor Green
Write-Host "  Run 'ironmem status' to verify." -ForegroundColor Cyan
Write-Host ""
