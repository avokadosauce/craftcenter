<#
.SYNOPSIS
    Builds and packages CraftCenter and craftcenter-cli for Windows.

.DESCRIPTION
    Produces a single asset: craftcenter-<version>-windows-<arch>-portable.zip,
    containing both binaries, the project's top-level docs, and a short
    portable.txt explaining where CraftCenter keeps its settings.

    There is deliberately no .msi here. CraftCenter installs entirely
    per-user and never elevates, so an MSI (which exists mainly to support
    machine-wide, elevated installs) would not be solving a problem this
    project has. The portable zip is the whole distribution on Windows.

.PARAMETER Arch
    Target architecture suffix used in the asset name. Only "x64" is
    currently supported, matching the x86_64-pc-windows-msvc Rust target
    this script builds.
#>
param(
    [string]$Arch = "x64"
)

$ErrorActionPreference = 'Stop'

$RustTarget = switch ($Arch) {
    'x64' { 'x86_64-pc-windows-msvc' }
    default { throw "packaging/windows/package.ps1: unsupported -Arch '$Arch' (expected 'x64')" }
}

$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$CargoTomlPath = Join-Path $Root 'Cargo.toml'

# Read the version the same way env.sh does: the first `version = "..."`
# line inside the [workspace.package] table. Kept independent of env.sh
# (which is bash) since this script has to run without one.
function Get-WorkspaceVersion {
    param([string]$CargoTomlPath)

    if ($env:CRAFTCENTER_VERSION) {
        return $env:CRAFTCENTER_VERSION
    }

    $inSection = $false
    foreach ($line in Get-Content -LiteralPath $CargoTomlPath) {
        if ($line -match '^\[workspace\.package\]') {
            $inSection = $true
            continue
        }
        if ($line -match '^\[') {
            $inSection = $false
            continue
        }
        if ($inSection -and ($line -match '^version\s*=\s*"([^"]*)"')) {
            return $Matches[1]
        }
    }

    throw "packaging/windows/package.ps1: could not read [workspace.package] version from $CargoTomlPath"
}

$Version = Get-WorkspaceVersion -CargoTomlPath $CargoTomlPath
$Dist = if ($env:DIST) { $env:DIST } else { Join-Path $Root 'dist\release' }
$CargoTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }

New-Item -ItemType Directory -Force -Path $Dist | Out-Null
New-Item -ItemType Directory -Force -Path $CargoTargetDir | Out-Null

# Stage into a scratch directory under the target dir (not $env:TEMP):
# keeping build scratch next to everything else Cargo produces, rather
# than on whatever drive the system temp folder happens to live on, is
# one less thing to differ between the box this was authored on and the
# Windows runner that will actually run it.
$Work = Join-Path $CargoTargetDir ("windows-package-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $Work | Out-Null
try {
    Push-Location $Root
    try {
        & cargo build --release --locked --target $RustTarget -p craftcenter -p craftcenter-cli
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }

    $ReleaseDir = Join-Path $CargoTargetDir "$RustTarget\release"
    $StageName = "craftcenter-$Version-windows-$Arch-portable"
    $Stage = Join-Path $Work $StageName
    New-Item -ItemType Directory -Force -Path $Stage | Out-Null

    Copy-Item (Join-Path $ReleaseDir 'craftcenter.exe') (Join-Path $Stage 'craftcenter.exe')
    Copy-Item (Join-Path $ReleaseDir 'craftcenter-cli.exe') (Join-Path $Stage 'craftcenter-cli.exe')

    foreach ($doc in @('README.md', 'LICENSE.md', 'NOTICE', 'ATTRIBUTION.md')) {
        $docPath = Join-Path $Root $doc
        if (Test-Path -LiteralPath $docPath) {
            Copy-Item $docPath (Join-Path $Stage $doc)
        }
    }

    $portableNotice = @"
CraftCenter — portable build

This copy of CraftCenter runs entirely from this folder. While
craftcenter.exe stays next to this file, the program keeps its settings
and installed-app records in a CraftCenterData folder created alongside
it (not in %APPDATA% or the registry), so the whole thing can be moved,
copied onto removable media, or deleted without leaving anything behind
elsewhere on the machine.

Move craftcenter.exe away from this folder and CraftCenterData, and it
will start a fresh CraftCenterData wherever it ends up instead.
"@
    Set-Content -LiteralPath (Join-Path $Stage 'portable.txt') -Value $portableNotice -Encoding utf8

    $ZipPath = Join-Path $Dist "$StageName.zip"
    if (Test-Path -LiteralPath $ZipPath) {
        Remove-Item -LiteralPath $ZipPath -Force
    }
    Compress-Archive -Path $Stage -DestinationPath $ZipPath

    Write-Host "packaging/windows/package.ps1: wrote $ZipPath"
}
finally {
    Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
}
