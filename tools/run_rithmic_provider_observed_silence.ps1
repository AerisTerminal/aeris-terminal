param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("heartbeat-silence", "message-silence")]
    [string]$ExpectedSilence,

    [Parameter(Mandatory = $true)]
    [ValidateRange(30, 86400)]
    [int]$ObservationSeconds,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$outputParent = Split-Path -Parent $OutputPath
if ([string]::IsNullOrWhiteSpace($outputParent)) {
    $outputParent = (Get-Location).Path
}
$resolvedParent = Resolve-Path -LiteralPath $outputParent
$absoluteOutput = Join-Path $resolvedParent.Path (Split-Path -Leaf $OutputPath)
if (Test-Path -LiteralPath $absoluteOutput) {
    throw "The evidence output already exists; provider evidence is never overwritten."
}

Push-Location $repoRoot
try {
    $worktreeState = @(git status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $worktreeState.Count -ne 0) {
        throw "Provider evidence requires a clean Git worktree, including no untracked files."
    }
    $sourceRevision = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $sourceRevision -notmatch '^[0-9a-f]{40}$') {
        throw "The exact 40-hex source revision could not be resolved."
    }

    $evidenceTarget = Join-Path $repoRoot "target\provider-silence-evidence"
    cargo build `
        --manifest-path (Join-Path $repoRoot "Cargo.toml") `
        --locked `
        --target-dir $evidenceTarget `
        --package asceify_rithmic_protocol_adapter `
        --bin rithmic_test_smoke
    if ($LASTEXITCODE -ne 0) {
        throw "The immutable provider-evidence binary did not build."
    }

    $worktreeState = @(git status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $worktreeState.Count -ne 0) {
        throw "The worktree changed during the evidence build."
    }
    $binary = Join-Path $evidenceTarget "debug\rithmic_test_smoke.exe"
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "The exact provider-evidence executable is missing."
    }
    $executableSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLowerInvariant()
    $cargoLockPath = Join-Path $repoRoot "Cargo.lock"
    $cargoLockSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $cargoLockPath).Hash.ToLowerInvariant()

    & $binary `
        --provider-observed-silence-evidence `
        $ExpectedSilence `
        $ObservationSeconds `
        $absoluteOutput `
        $sourceRevision `
        $executableSha256 `
        $cargoLockSha256
    if ($LASTEXITCODE -ne 0) {
        throw "Provider-path Rithmic silence evidence did not qualify. Inspect the fail-closed artifact."
    }

    $finalRevision = (git rev-parse HEAD).Trim()
    $finalWorktreeState = @(git status --porcelain=v1 --untracked-files=all)
    $finalExecutableSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $binary).Hash.ToLowerInvariant()
    $finalCargoLockSha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $cargoLockPath).Hash.ToLowerInvariant()
    if ($LASTEXITCODE -ne 0 `
        -or $finalWorktreeState.Count -ne 0 `
        -or $finalRevision -cne $sourceRevision `
        -or $finalExecutableSha256 -cne $executableSha256 `
        -or $finalCargoLockSha256 -cne $cargoLockSha256) {
        throw "Source, executable, or Cargo.lock provenance changed during observation."
    }

    & $binary `
        --verify-provider-observed-silence-evidence `
        $absoluteOutput `
        $sourceRevision `
        $cargoLockSha256
    if ($LASTEXITCODE -ne 0) {
        throw "Provider-path Rithmic silence evidence verification failed."
    }
}
finally {
    Pop-Location
}
