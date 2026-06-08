# Locate pkg-config on Windows CI. Prefer an existing install (e.g. Strawberry Perl);
# otherwise install pkgconfiglite via Chocolatey (HTTP package needs allow-empty-checksums).
$ErrorActionPreference = 'Stop'

if (Get-Command pkg-config -ErrorAction SilentlyContinue) {
    $exe = (Get-Command pkg-config).Source
} else {
    choco install pkgconfiglite -y --no-progress --allow-empty-checksums
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
    $exe = (Get-Command pkg-config -ErrorAction Stop).Source
}

Write-Host "Using PKG_CONFIG=$exe"
Add-Content -Path $env:GITHUB_ENV -Value "PKG_CONFIG=$exe"
