#!/usr/bin/env pwsh
[CmdletBinding()]
param([switch]$Release, [switch]$SkipCopy, [switch]$Force, [string]$Bindgen = $env:WASM_BINDGEN)
$ErrorActionPreference = 'Stop'
$arguments = @((Join-Path $PSScriptRoot 'build-web.cjs'))
if ($Release) { $arguments += '--release' }
if ($SkipCopy) { $arguments += '--skip-copy' }
if ($Force) { $arguments += '--force' }
if ($Bindgen) { $arguments += @('--bindgen', $Bindgen) }
& node @arguments
if ($LASTEXITCODE -ne 0) { throw 'Glory frontend build failed' }
