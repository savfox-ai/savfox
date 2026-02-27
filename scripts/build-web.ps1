#!/usr/bin/env pwsh
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$SkipCopy
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDir "..")).Path
$webDir = Join-Path $repoRoot "crates/gateway-dixous"

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
    
    if (Test-Path $staticDir) {
        Write-Host "==> Removing existing static folder"
        Remove-Item -Recurse -Force $staticDir
    }
    
    Write-Host "==> Copying build output to static folder"
    New-Item -ItemType Directory -Path $staticDir -Force | Out-Null
    Copy-Item -Recurse -Force (Join-Path $buildPath "*") $staticDir
    
    Write-Host "==> Web frontend ready at $staticDir"
}

Pop-Location
