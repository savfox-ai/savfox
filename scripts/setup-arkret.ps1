param(
    [string]$WorkspaceRoot = (Split-Path -Parent $PSScriptRoot)
)

$ErrorActionPreference = 'Stop'
$dependencyRoot = Join-Path (Split-Path -Parent (Split-Path -Parent $WorkspaceRoot)) 'arkret-org'
$repositories = @(
    @{ Name = 'arkret-rust-sdk'; Revision = 'f4aae2269746187661461b30856dfec22354fff7' },
    @{ Name = 'garth'; Revision = 'cb3d1d4f31897780413da8474333f47f87bd32c7' },
    @{ Name = 'bridges'; Revision = '724d98de6d8b7f1de8fbd2df99647d0990e1c5d5' }
)

foreach ($repository in $repositories) {
    $destination = Join-Path $dependencyRoot $repository.Name
    if (Test-Path -LiteralPath $destination) {
        $head = git -C $destination rev-parse HEAD
        if ($LASTEXITCODE -ne 0 -or $head -ne $repository.Revision) {
            throw "Existing checkout at $destination must be at $($repository.Revision); it was left unchanged."
        }
        $changes = git -C $destination status --porcelain
        if ($LASTEXITCODE -ne 0 -or $changes) {
            throw "Existing checkout at $destination has local changes; it was left unchanged."
        }
        continue
    }

    git init --quiet $destination
    if ($LASTEXITCODE -ne 0) { throw "Failed to initialize $destination" }
    git -C $destination remote add origin "https://github.com/arkret-org/$($repository.Name).git"
    if ($LASTEXITCODE -ne 0) { throw "Failed to configure $destination" }
    git -C $destination fetch --depth 1 origin $repository.Revision
    if ($LASTEXITCODE -ne 0) { throw "Failed to fetch $($repository.Name) at $($repository.Revision)" }
    git -C $destination checkout --quiet --detach FETCH_HEAD
    if ($LASTEXITCODE -ne 0) { throw "Failed to check out $($repository.Name)" }
}
