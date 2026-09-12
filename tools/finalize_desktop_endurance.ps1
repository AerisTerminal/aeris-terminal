<#
.SYNOPSIS
Finalizes and verifies a stopped, completed desktop-endurance capture.

.DESCRIPTION
Normally the supervisor invokes this tool after observing exit code zero. It can
also recover a completed report after supervisor loss. Recovery is explicit in
the manifest and never invents an observed process exit code.

.EXAMPLE
powershell -File local-data/evidence/desktop-endurance/finalize_desktop_endurance.ps1 -ManifestPath local-data/evidence/desktop-endurance/run-manifest.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Get-RequiredProperty {
    param([object]$Object, [string]$Name)
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "Missing required property '$Name'."
    }
    return $property.Value
}

function Write-JsonAtomically {
    param([string]$Path, [object]$Value)
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

$resolvedManifestPath = [IO.Path]::GetFullPath($ManifestPath)
$manifest = Get-Content -LiteralPath $resolvedManifestPath -Raw | ConvertFrom-Json
Assert-True ((Get-RequiredProperty $manifest "schema_version") -eq 2) "Finalization requires a schema-2 manifest."
Assert-True (-not [bool](Get-RequiredProperty $manifest "finalized")) "Desktop-endurance manifest is already finalized."

$pidValue = Get-RequiredProperty $manifest "pid"
$parsedPid = 0L
Assert-True ($pidValue -isnot [string] -and [Int64]::TryParse([string]$pidValue, [ref]$parsedPid) -and $parsedPid -gt 0 -and $parsedPid -le [int]::MaxValue) "Manifest pid is invalid."
Assert-True ($null -eq (Get-Process -Id ([int]$parsedPid) -ErrorAction SilentlyContinue)) "Desktop-endurance process $parsedPid is still running."

$sourceRevision = [string](Get-RequiredProperty $manifest "commit")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Manifest source revision is invalid."

foreach ($scriptName in @("supervisor", "finalizer", "verifier")) {
    $scriptPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "${scriptName}_script_path"))
    $expectedScriptHash = [string](Get-RequiredProperty $manifest "${scriptName}_script_sha256")
    Assert-True (Test-Path -LiteralPath $scriptPath -PathType Leaf) "Captured $scriptName script is missing."
    Assert-True ((Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash -ieq $expectedScriptHash) "Captured $scriptName script hash changed."
    if ($scriptName -eq "finalizer") {
        Assert-True ($scriptPath -eq [IO.Path]::GetFullPath($PSCommandPath)) "Finalization must use the captured finalizer script."
    }
}

$cargoLockPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "cargo_lock_path"))
$binaryPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "binary_path"))
$reportPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "report_path"))
$stdoutPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "stdout_path"))
$stderrPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "stderr_path"))
foreach ($path in @($cargoLockPath, $binaryPath, $reportPath, $stdoutPath, $stderrPath)) {
    Assert-True (Test-Path -LiteralPath $path -PathType Leaf) "Required endurance artifact is missing: $path"
}
$expectedCargoLockHash = [string](Get-RequiredProperty $manifest "cargo_lock_sha256")
$expectedBinaryHash = [string](Get-RequiredProperty $manifest "binary_sha256")
Assert-True ((Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash -ieq $expectedCargoLockHash) "Captured Cargo.lock hash changed."
Assert-True ((Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash -ieq $expectedBinaryHash) "Captured executable hash changed."

$report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
Assert-True ((Get-RequiredProperty $report "schema_version") -eq 2) "Desktop-endurance report schema is invalid."
Assert-True ((Get-RequiredProperty $report "evidence_scope") -eq "synthetic_headless_desktop_continuous_endurance") "Desktop-endurance report has the wrong evidence scope."
Assert-True ((Get-RequiredProperty $report "completion_state") -eq "completed") "Desktop-endurance report is incomplete."
Assert-True ((Get-RequiredProperty $report "clean_stop") -is [bool] -and [bool](Get-RequiredProperty $report "clean_stop")) "Desktop-endurance report does not record a clean stop."
Assert-True ((Get-RequiredProperty $report "synthetic_bounds_qualified") -is [bool] -and [bool](Get-RequiredProperty $report "synthetic_bounds_qualified")) "Desktop-endurance synthetic bounds are not qualified."
$liveMarketGate = [string](Get-RequiredProperty $report "live_market_gate")
Assert-True ($liveMarketGate -eq "not_run" -or $liveMarketGate -eq "passed" -or $liveMarketGate -eq "failed") "Desktop-endurance live market gate value is invalid."

$launchMode = [string](Get-RequiredProperty $manifest "launch_mode")
Assert-True ($launchMode -eq "foreground_supervisor" `
    -or $launchMode -eq "wmi_detached_interactive_session") "Desktop-endurance launch mode is invalid."

$exitEvidence = Get-RequiredProperty $manifest "process_exit_evidence"
Assert-True ((Get-RequiredProperty $manifest "system_sleep_inhibited") -is [bool] `
    -and [bool](Get-RequiredProperty $manifest "system_sleep_inhibited")) "Desktop-endurance system sleep was not inhibited."
if ($exitEvidence -eq "supervisor_observed_zero") {
    Assert-True ((Get-RequiredProperty $manifest "process_exit_code") -eq 0) "Supervisor did not observe exit code zero."
    Assert-True ((Get-RequiredProperty $manifest "system_sleep_inhibition_released") -is [bool] `
        -and [bool](Get-RequiredProperty $manifest "system_sleep_inhibition_released") `
        -and (Get-RequiredProperty $manifest "sleep_inhibition_release_evidence") -eq "explicit_es_continuous") "Supervisor did not explicitly restore its execution state."
    $manifest.finalization_mode = "supervised"
}
elseif ($null -eq $exitEvidence) {
    Assert-True ($null -eq (Get-RequiredProperty $manifest "process_exit_code")) "Recovery cannot discard an observed exit code."
    $supervisorPidValue = Get-RequiredProperty $manifest "supervisor_pid"
    $supervisorPid = 0L
    Assert-True ($supervisorPidValue -isnot [string] `
        -and [Int64]::TryParse([string]$supervisorPidValue, [ref]$supervisorPid) `
        -and $supervisorPid -gt 0 `
        -and $supervisorPid -le [int]::MaxValue) "Recovery supervisor_pid is invalid."
    Assert-True ($null -eq (Get-Process -Id ([int]$supervisorPid) -ErrorAction SilentlyContinue)) "Recovery supervisor is still running."
    $manifest.process_exit_evidence = "completed_report_recovery"
    $manifest.finalization_mode = "recovered_after_supervisor_loss"
    $manifest.system_sleep_inhibition_released = $true
    $manifest.sleep_inhibition_release_evidence = "supervisor_thread_terminated"
}
else {
    throw "Desktop-endurance exit evidence does not permit finalization."
}

$manifest.stdout_sha256 = (Get-FileHash -LiteralPath $stdoutPath -Algorithm SHA256).Hash
$manifest.stderr_sha256 = (Get-FileHash -LiteralPath $stderrPath -Algorithm SHA256).Hash
$manifest.report_sha256 = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
$manifest.finalized_utc = [DateTimeOffset]::UtcNow.ToString("O")
$manifest.finalized = $true
Write-JsonAtomically $resolvedManifestPath $manifest

$verifier = Join-Path $PSScriptRoot "verify_desktop_endurance.ps1"
& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $verifier -ManifestPath $resolvedManifestPath
if ($LASTEXITCODE -ne 0) {
    $manifest.finalized = $false
    $manifest.report_sha256 = $null
    $manifest.stdout_sha256 = $null
    $manifest.stderr_sha256 = $null
    Write-JsonAtomically $resolvedManifestPath $manifest
    throw "Desktop-endurance artifacts failed final verification."
}

Write-Output "desktop_endurance_capture=verified finalization_mode=$($manifest.finalization_mode) report=$reportPath manifest=$resolvedManifestPath"
