# Stage MSVC OpenMP runtime (vcomp140.dll) next to the release binary.
#
# Uses the desktop Redist layout only:
#   VC\Redist\MSVC\<version>\<arch>\Microsoft.VC143.OpenMP\vcomp140.dll
# Skips the onecore copy under:
#   VC\Redist\MSVC\<version>\onecore\<arch>\...
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

function Collect-Vcomp140FromRedistRoot {
    param(
        [string]$RedistRoot,
        [string]$ArchDir,
        [System.Collections.Generic.List[System.IO.FileInfo]]$Out
    )

    if (-not (Test-Path $RedistRoot)) { return }

    foreach ($versionDir in Get-ChildItem $RedistRoot -Directory -ErrorAction SilentlyContinue) {
        if ($versionDir.Name -eq 'onecore') { continue }

        $openmpGlob = Join-Path $versionDir.FullName "$ArchDir\Microsoft.VC*.OpenMP"
        foreach ($openmpDir in Get-ChildItem $openmpGlob -Directory -ErrorAction SilentlyContinue) {
            $dll = Join-Path $openmpDir.FullName 'vcomp140.dll'
            if (Test-Path $dll) {
                $Out.Add((Get-Item -LiteralPath $dll))
            }
        }
    }
}

function Find-Vcomp140Dll {
    param([string]$ArchDir)

    $candidates = [System.Collections.Generic.List[System.IO.FileInfo]]::new()

    foreach ($pf in @($env:ProgramFiles, ${env:ProgramFiles(x86)})) {
        $vsRoot = Join-Path $pf 'Microsoft Visual Studio'
        if (-not (Test-Path $vsRoot)) { continue }

        foreach ($versionRoot in Get-ChildItem $vsRoot -Directory -ErrorAction SilentlyContinue) {
            foreach ($flavor in Get-ChildItem $versionRoot.FullName -Directory -ErrorAction SilentlyContinue) {
                $redist = Join-Path $flavor.FullName 'VC\Redist\MSVC'
                Collect-Vcomp140FromRedistRoot -RedistRoot $redist -ArchDir $ArchDir -Out $candidates
            }
        }
    }

    if ($env:VCToolsInstallDir) {
        $redist = Join-Path $env:VCToolsInstallDir '..\..\Redist\MSVC'
        Collect-Vcomp140FromRedistRoot -RedistRoot $redist -ArchDir $ArchDir -Out $candidates
    }

    if ($candidates.Count -eq 0) { return $null }

    return ($candidates | Sort-Object FullName -Descending | Select-Object -First 1).FullName
}

$src = Find-Vcomp140Dll -ArchDir $Arch
if (-not $src) {
    Write-Error "vcomp140.dll for $Arch not found under Visual Studio Redist OpenMP folders."
}

$dest = Join-Path $StagingDir 'vcomp140.dll'
Copy-Item -LiteralPath $src -Destination $dest -Force
Write-Host "Bundled OpenMP runtime: $src -> $dest"
