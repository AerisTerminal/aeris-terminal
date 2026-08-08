<#
.SYNOPSIS
Builds and launches one provenance-bound physical native-transition capture.

.DESCRIPTION
This launcher never changes network or power state. After the Rithmic Test chart
is fully ready, the operator performs one physical online-to-offline-to-online
cycle and one physical suspend/resume cycle. The existing shipping worker loads
credentials only from the native vault. Close the application normally after
both scenarios rehydrate; the worker then records its clean final stop.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$HistoryRoot,

    [Parameter(Mandatory = $true)]
    [string]$ReportPath,

    [switch]$DetailedDiagnostics
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Write-JsonAtomically {
    param([string]$Path, [object]$Value)
    $parent = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        $null = New-Item -ItemType Directory -Path $parent -Force
    }
    $temporary = "$Path.$PID.partial"
    try {
        [IO.File]::WriteAllText(
            $temporary,
            ($Value | ConvertTo-Json -Depth 8) + [Environment]::NewLine
        )
        Move-Item -LiteralPath $temporary -Destination $Path -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$resolvedHistoryRoot = [IO.Path]::GetFullPath($HistoryRoot)
$resolvedReportPath = [IO.Path]::GetFullPath($ReportPath)
$manifestPath = "$resolvedReportPath.manifest.json"
$cargoLockPath = Join-Path $repoRoot "Cargo.lock"
if ((Test-Path -LiteralPath $resolvedReportPath) -or (Test-Path -LiteralPath $manifestPath)) {
    throw "Native-transition report and manifest paths must both be new."
}

$status = @(git -C $repoRoot status --porcelain=v1 --untracked-files=all)
if ($LASTEXITCODE -ne 0 -or $status.Count -ne 0) {
    throw "Native-transition evidence requires a clean Git worktree."
}
$sourceRevision = (git -C $repoRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceRevision -notmatch '^[0-9a-fA-F]{40}$') {
    throw "Native-transition evidence requires a full source revision."
}

cargo build --manifest-path (Join-Path $repoRoot "Cargo.toml") --locked --release --package axiusflow_desktop --all-features
if ($LASTEXITCODE -ne 0) {
    throw "Native-transition release build failed."
}
$executablePath = Join-Path $repoRoot "target\release\axiusflow_desktop.exe"
$executablePath = (Resolve-Path -LiteralPath $executablePath).Path
$cargoLockPath = (Resolve-Path -LiteralPath $cargoLockPath).Path
$manifest = [ordered]@{
    schema_version = 1
    evidence_scope = "rithmic_test_native_transition_capture_manifest"
    source_revision = $sourceRevision
    clean_worktree = $true
    cargo_lock_path = $cargoLockPath
    cargo_lock_sha256 = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
    executable_path = $executablePath
    executable_sha256 = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
    report_path = $resolvedReportPath
    started_utc = [DateTimeOffset]::UtcNow.ToString("O")
    finalized = $false
    final_report_sha256 = $null
    process_exit_code = $null
    finalized_utc = $null
}
Write-JsonAtomically $manifestPath $manifest

$arguments = @(
    "--rithmic-test",
    $resolvedHistoryRoot,
    "--capture-native-transitions",
    $resolvedReportPath
)
if ($DetailedDiagnostics) {
    $arguments += "--detailed-diagnostics"
}
& $executablePath @arguments
$processExitCode = $LASTEXITCODE

$manifest.process_exit_code = $processExitCode
$manifest.finalized_utc = [DateTimeOffset]::UtcNow.ToString("O")
$finalStatus = @(git -C $repoRoot status --porcelain=v1 --untracked-files=all)
$finalRevision = (git -C $repoRoot rev-parse HEAD).Trim()
$finalCargoLockHash = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
$finalExecutableHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
$provenanceStable = $LASTEXITCODE -eq 0 `
    -and $finalStatus.Count -eq 0 `
    -and $finalRevision -eq $sourceRevision `
    -and $finalCargoLockHash -eq $manifest.cargo_lock_sha256 `
    -and $finalExecutableHash -eq $manifest.executable_sha256
if ($processExitCode -ne 0 `
    -or -not $provenanceStable `
    -or -not (Test-Path -LiteralPath $resolvedReportPath -PathType Leaf)) {
    Write-JsonAtomically $manifestPath $manifest
    throw "Native-transition capture did not finish with stable provenance and a clean process exit."
}
$manifest.final_report_sha256 =
    (Get-FileHash -LiteralPath $resolvedReportPath -Algorithm SHA256).Hash
$manifest.finalized = $true
Write-JsonAtomically $manifestPath $manifest

$verifier = Join-Path $PSScriptRoot "verify_native_transition_capture.ps1"
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $verifier `
    -ArtifactPath $resolvedReportPath `
    -ManifestPath $manifestPath
if ($LASTEXITCODE -ne 0) {
    throw "Native-transition capture artifacts failed final verification."
}
Write-Output "native_transition_capture_process=completed report=$resolvedReportPath manifest=$manifestPath"
