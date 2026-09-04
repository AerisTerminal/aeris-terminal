<#
.SYNOPSIS
Builds and launches one provenance-bound physical native-transition capture.

.DESCRIPTION
This launcher never changes network or power state. It builds while online, then
pauses so the operator can disconnect before the application starts. After the
application records the real offline initial probe, reconnect, wait for the chart
and DOM to become ready, perform a separate physical online-to-offline-to-online
cycle, then perform one physical suspend/resume cycle. Close the application
normally only after both later scenarios rehydrate. The existing shipping worker
loads credentials only from the native vault.
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

Write-Output "native_transition_operator_step=disconnect_network_before_start"
Write-Output "native_transition_operator_sequence=launch_offline,reconnect_until_ready,separate_offline_recovery_until_ready,suspend_resume_until_ready,close_normally"
$null = Read-Host "Physically disconnect all network access, then press Enter to launch the application offline"

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

Write-Output "native_transition_operator_step=launching_offline reconnect only after the application shows its offline startup state"
Write-Output "native_transition_operator_step=after_first_ready perform a separate network loss/recovery, then suspend/resume, waiting for full chart and DOM recovery after each"
# The capture command takes its paths explicitly: the binary cannot
# reliably discover the repository it was built from, while the lock file
# lives at the repository root by definition.
$arguments = @(
    "--capture-native-transitions",
    $resolvedHistoryRoot,
    $resolvedReportPath,
    $cargoLockPath,
    $executablePath
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
Write-Output "native_transition_capture_process=completed physical_offline_startup=true physical_network_offline=true physical_suspend_resume=true report=$resolvedReportPath manifest=$manifestPath"
