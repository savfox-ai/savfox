#!/usr/bin/env pwsh
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$SkipCopy,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-DioxusOutDir {
    param(
        [Parameter(Mandatory = $true)][string]$DioxusTomlPath
    )

    if (-not (Test-Path $DioxusTomlPath)) {
        return "dist"
    }

    $inApplicationSection = $false
    foreach ($line in Get-Content -Path $DioxusTomlPath) {
        if ($line -match '^\s*\[([^\]]+)\]\s*$') {
            $inApplicationSection = ($matches[1].Trim() -eq "application")
            continue
        }

        if ($inApplicationSection -and $line -match '^\s*out_dir\s*=\s*"([^"]+)"') {
            return $matches[1]
        }
    }

    return "dist"
}

function Get-TrackedFiles {
    param(
        [Parameter(Mandatory = $true)][string[]]$Paths
    )

    foreach ($path in $Paths) {
        if (-not (Test-Path $path)) {
            continue
        }

        $item = Get-Item $path
        if ($item.PSIsContainer) {
            Get-ChildItem -Path $path -Recurse -File
            continue
        }

        $item
    }
}

function Get-InputFingerprint {
    param(
        [Parameter(Mandatory = $true)][string]$RepoRoot,
        [Parameter(Mandatory = $true)][string[]]$Paths
    )

    $builder = New-Object System.Text.StringBuilder
    $files = Get-TrackedFiles -Paths $Paths | Sort-Object FullName -Unique

    foreach ($file in $files) {
        $relativePath = [System.IO.Path]::GetRelativePath($RepoRoot, $file.FullName)
        [void]$builder.AppendLine(("{0}|{1}|{2}" -f $relativePath.Replace("\", "/"), $file.Length, $file.LastWriteTimeUtc.Ticks))
    }

    if ($builder.Length -eq 0) {
        throw "Could not determine frontend build inputs"
    }

    $bytes = [System.Text.Encoding]::UTF8.GetBytes($builder.ToString())
    $hashBytes = [System.Security.Cryptography.SHA256]::HashData($bytes)
    return [Convert]::ToHexString($hashBytes)
}

function Test-StampMatches {
    param(
        [Parameter(Mandatory = $true)][string]$StampPath,
        [Parameter(Mandatory = $true)][string]$Fingerprint
    )

    if (-not (Test-Path $StampPath)) {
        return $false
    }

    return ((Get-Content -Path $StampPath -Raw).Trim() -eq $Fingerprint)
}

function Write-Stamp {
    param(
        [Parameter(Mandatory = $true)][string]$StampPath,
        [Parameter(Mandatory = $true)][string]$Fingerprint
    )

    $parent = Split-Path -Parent $StampPath
    if ($parent -and -not (Test-Path $parent)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }

    Set-Content -Path $StampPath -Value $Fingerprint -NoNewline
}

function Test-FrontendOutputReady {
    param(
        [Parameter(Mandatory = $true)][string]$OutputPath
    )

    $requiredPaths = @(
        (Join-Path $OutputPath "index.html"),
        (Join-Path $OutputPath "wasm")
    )

    foreach ($requiredPath in $requiredPaths) {
        if (-not (Test-Path $requiredPath)) {
            return $false
        }
    }

    return $true
}

function Sync-BuildOutput {
    param(
        [Parameter(Mandatory = $true)][string]$SourcePath,
        [Parameter(Mandatory = $true)][string]$DestinationPath,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $sourceResolved = (Resolve-Path $SourcePath).Path
    $destResolved = $null

    if (Test-Path $DestinationPath) {
        $destResolved = (Resolve-Path $DestinationPath).Path
    }

    if ($destResolved -and [string]::Equals($sourceResolved, $destResolved, [StringComparison]::OrdinalIgnoreCase)) {
        Write-Host "==> Skipping copy to $Label (already at $destResolved)"
        return $false
    }

    if (Test-Path $DestinationPath) {
        Write-Host "==> Removing existing $Label"
        Remove-Item -Recurse -Force $DestinationPath
    }

    New-Item -ItemType Directory -Path $DestinationPath -Force | Out-Null
    Copy-Item -Recurse -Force (Join-Path $SourcePath "*") $DestinationPath
    Write-Host "==> Synced $Label to $DestinationPath"
    return $true
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$webDir = Join-Path $repoRoot "crates/gateway-dioxus"
$dioxusTomlPath = Join-Path $webDir "Dioxus.toml"
$staticDir = Join-Path $repoRoot "crates/gateway-server/static"
$frontendOutDirName = Get-DioxusOutDir -DioxusTomlPath $dioxusTomlPath
$frontendOutDir = Join-Path $webDir $frontendOutDirName
$profileName = if ($Release) { "release" } else { "debug" }
$cacheDir = Join-Path $repoRoot "target/web-build-cache"
$frontendStampPath = Join-Path $cacheDir "$profileName-frontend.stamp"
$staticStampPath = Join-Path $cacheDir "$profileName-static.stamp"
$trackedInputs = @(
    (Join-Path $webDir "src"),
    (Join-Path $webDir "assets"),
    (Join-Path $webDir "Cargo.toml"),
    $dioxusTomlPath,
    (Join-Path $repoRoot "Cargo.lock"),
    (Join-Path $repoRoot "crates/gateway-shared/Cargo.toml"),
    (Join-Path $repoRoot "crates/gateway-shared/src"),
    (Join-Path $repoRoot "crates/utils/Cargo.toml"),
    (Join-Path $repoRoot "crates/utils/src")
)
$fingerprint = Get-InputFingerprint -RepoRoot $repoRoot -Paths $trackedInputs
$needsWebBuild = $Force -or -not (Test-StampMatches -StampPath $frontendStampPath -Fingerprint $fingerprint) -or -not (Test-FrontendOutputReady -OutputPath $frontendOutDir)
$buildPath = $frontendOutDir

if ($needsWebBuild) {
    Push-Location $webDir

    try {
        if (Test-Path $frontendOutDir) {
            Remove-Item -Recurse -Force $frontendOutDir
        }

        $dxArgs = @("build", "--web")
        if ($Release) {
            $dxArgs += "--release"
        }

        Write-Host "==> Building web frontend ($($dxArgs -join ' '))"
        $dxOutput = & dx @dxArgs 2>&1 | Out-String
        Write-Host $dxOutput

        if (-not $dxOutput) {
            throw "dx build produced no output"
        }

        $pattern = 'path="([^"]+)"'
        $match = [regex]::Match($dxOutput, $pattern)
        if (-not $match.Success) {
            throw "Could not find output path in dx output: $dxOutput"
        }

        $buildPath = $match.Groups[1].Value
        Write-Host "==> Build output: $buildPath"

        if (-not (Test-Path $buildPath)) {
            throw "Build path does not exist: $buildPath"
        }
    }
    finally {
        Pop-Location
    }
}
else {
    Write-Host "==> Web frontend is up to date; skipping dx build"
}

if ($needsWebBuild) {
    Sync-BuildOutput -SourcePath $buildPath -DestinationPath $frontendOutDir -Label "frontend out_dir" | Out-Null
    Write-Stamp -StampPath $frontendStampPath -Fingerprint $fingerprint
}

if (-not $SkipCopy) {
    $needsStaticSync = $Force -or $needsWebBuild -or -not (Test-StampMatches -StampPath $staticStampPath -Fingerprint $fingerprint) -or -not (Test-FrontendOutputReady -OutputPath $staticDir)

    if ($needsStaticSync) {
        Sync-BuildOutput -SourcePath $buildPath -DestinationPath $staticDir -Label "gateway static folder" | Out-Null
        Write-Stamp -StampPath $staticStampPath -Fingerprint $fingerprint
    }
    else {
        Write-Host "==> Gateway static folder is up to date; skipping copy"
    }

    Write-Host "==> Web frontend ready"
    Write-Host "    - static: $staticDir"
    Write-Host "    - out_dir: $frontendOutDir"
}
