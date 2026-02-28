#!/usr/bin/env pwsh
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$SkipCopy
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
        return
    }

    if (Test-Path $DestinationPath) {
        Write-Host "==> Removing existing $Label"
        Remove-Item -Recurse -Force $DestinationPath
    }

    New-Item -ItemType Directory -Path $DestinationPath -Force | Out-Null
    Copy-Item -Recurse -Force (Join-Path $SourcePath "*") $DestinationPath
    Write-Host "==> Synced $Label to $DestinationPath"
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$webDir = Join-Path $repoRoot "crates/gateway-dixous"
$dioxusTomlPath = Join-Path $webDir "Dioxus.toml"

Push-Location $webDir

$dxArgs = @("build", "--web")
if ($Release) {
    $dxArgs += "--release"
}

Write-Host "==> Building web frontend ($($dxArgs -join ' '))"
$dxOutput = & dx @dxArgs 2>&1 | Out-String
Write-Host $dxOutput

if (-not $dxOutput) {
    Pop-Location
    throw "dx build produced no output"
}

$pattern = 'path="([^"]+)"'
$match = [regex]::Match($dxOutput, $pattern)
if (-not $match.Success) {
    Pop-Location
    throw "Could not find output path in dx output: $dxOutput"
}

$buildPath = $match.Groups[1].Value
Write-Host "==> Build output: $buildPath"

if (-not (Test-Path $buildPath)) {
    Pop-Location
    throw "Build path does not exist: $buildPath"
}

if (-not $SkipCopy) {
    $staticDir = Join-Path $repoRoot "crates/gateway-server/static"
    $frontendOutDirName = Get-DioxusOutDir -DioxusTomlPath $dioxusTomlPath
    $frontendOutDir = Join-Path $webDir $frontendOutDirName

    Sync-BuildOutput -SourcePath $buildPath -DestinationPath $staticDir -Label "gateway static folder"
    Sync-BuildOutput -SourcePath $buildPath -DestinationPath $frontendOutDir -Label "frontend out_dir"

    Write-Host "==> Web frontend ready"
    Write-Host "    - static: $staticDir"
    Write-Host "    - out_dir: $frontendOutDir"
}

Pop-Location
