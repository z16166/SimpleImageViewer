# Stage MSVC OpenMP runtime (vcomp140.dll) next to the release binary.
param(
    [Parameter(Mandatory = $true)]
    [string]$StagingDir,
    [Parameter(Mandatory = $true)]
    [ValidateSet('x64', 'x86', 'arm64')]
    [string]$Arch
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $StagingDir)) {
    New-Item -ItemType Directory -Force -Path $StagingDir | Out-Null
}

function Find-Vcomp140Dll {
    param([string]$ArchDir)

    $relative = "Microsoft.VC*.OpenMP\$ArchDir\vcomp140.dll"
    $roots = @(
        ${env:ProgramFiles(x86)} + '\Microsoft Visual Studio\2022',
        ${env:ProgramFiles} + '\Microsoft Visual Studio\2022'
    )

    foreach ($root in $roots) {
        if (-not (Test-Path $root)) { continue }
        $match = Get-ChildItem -Path $root -Recurse -Filter 'vcomp140.dll' -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -like "*\$relative" } |
            Sort-Object { $_.FullName.Length } -Descending |
            Select-Object -First 1
        if ($match) { return $match.FullName }
    }

    if ($env:VCToolsInstallDir) {
        $redistRoot = Join-Path $env:VCToolsInstallDir '..\..\Redist\MSVC'
        if (Test-Path $redistRoot) {
            $match = Get-ChildItem -Path $redistRoot -Recurse -Filter 'vcomp140.dll' -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -like "*\$relative" } |
                Sort-Object { $_.FullName.Length } -Descending |
                Select-Object -First 1
            if ($match) { return $match.FullName }
        }
    }

    return $null
}

$src = Find-Vcomp140Dll -ArchDir $Arch
if (-not $src) {
    Write-Error "vcomp140.dll for $Arch not found under Visual Studio Redist OpenMP folders."
}

$dest = Join-Path $StagingDir 'vcomp140.dll'
Copy-Item -LiteralPath $src -Destination $dest -Force
Write-Host "Bundled OpenMP runtime: $src -> $dest"
