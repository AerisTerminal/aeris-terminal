<#
.SYNOPSIS
Verifies a completed eight-hour desktop-endurance artifact without controlling its process.

.DESCRIPTION
The legacy schema-1 manifest contains the captured binary and report provenance.
Schema 2 additionally binds Cargo.lock, stdout, stderr, clean exit, and explicit
finalization. The report hash is added only after the process exits. Relative
artifact paths resolve from the manifest directory. Verification fails closed
while the recorded PID is running and never changes that process.

.EXAMPLE
powershell -File tools/verify_desktop_endurance.ps1 -ManifestPath local-data/evidence/endurance/run-manifest.json
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

function Resolve-ArtifactPath {
    param([string]$Path, [string]$BaseDirectory)
    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $BaseDirectory $Path))
}

function Read-JsonArtifact {
    param([string]$Path, [string]$Description)
    Assert-True (Test-Path -LiteralPath $Path -PathType Leaf) "$Description does not exist: $Path"
    try {
        return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    }
    catch {
        throw "$Description is not valid JSON: $Path"
    }
}

function Assert-UnsignedIntegerAtLeast {
    param([object]$Value, [uint64]$Minimum, [string]$Name)
    $converted = 0L
    Assert-True ($Value -isnot [string] -and $Value -isnot [bool] -and [Int64]::TryParse([string]$Value, [ref]$converted)) "$Name must be a JSON integer."
    Assert-True ($converted -ge 0 -and [uint64]$converted -ge $Minimum) "$Name is below its required minimum."
}

function Assert-JsonTrue {
    param([object]$Value, [string]$Name)
    Assert-True ($Value -is [bool] -and $Value) "$Name must be the JSON boolean true."
}

$resolvedManifestPath = [IO.Path]::GetFullPath($ManifestPath)
$manifestDirectory = Split-Path -Parent $resolvedManifestPath
$manifest = Read-JsonArtifact $resolvedManifestPath "Desktop endurance manifest"

$manifestSchema = Get-RequiredProperty $manifest "schema_version"
Assert-True ($manifestSchema -eq 1 -or $manifestSchema -eq 2) "Desktop endurance manifest schema_version must be 1 or 2."
if ($manifestSchema -eq 2) {
    Assert-True ((Get-RequiredProperty $manifest "evidence_scope") -eq "desktop_endurance_capture_manifest") "Desktop endurance manifest has the wrong evidence_scope."
    Assert-JsonTrue (Get-RequiredProperty $manifest "clean_worktree") "Manifest clean_worktree"
    Assert-JsonTrue (Get-RequiredProperty $manifest "finalized") "Manifest finalized"
    $exitEvidence = Get-RequiredProperty $manifest "process_exit_evidence"
    $finalizationMode = [string](Get-RequiredProperty $manifest "finalization_mode")
    if ($exitEvidence -eq "supervisor_observed_zero") {
        Assert-True ((Get-RequiredProperty $manifest "process_exit_code") -eq 0) "Desktop endurance supervisor did not observe exit code zero."
        Assert-True ($finalizationMode -eq "supervised") "Supervised desktop-endurance finalization mode is invalid."
    }
    elseif ($exitEvidence -eq "completed_report_recovery") {
        Assert-True ($null -eq (Get-RequiredProperty $manifest "process_exit_code")) "Recovered finalization must not invent a process exit code."
        Assert-True ($finalizationMode -eq "recovered_after_supervisor_loss") "Recovered desktop-endurance finalization mode is invalid."
    }
    else {
        throw "Desktop endurance process exit evidence is invalid."
    }
    $launchMode = [string](Get-RequiredProperty $manifest "launch_mode")
    Assert-True ($launchMode -eq "foreground_supervisor" `
        -or $launchMode -eq "wmi_detached_interactive_session") "Desktop endurance launch mode is invalid."
    $logoffResilient = Get-RequiredProperty $manifest "logoff_resilient"
    Assert-True ($logoffResilient -is [bool] -and -not $logoffResilient) "Desktop endurance must honestly record that the current launch modes do not survive Windows logoff."
    Assert-JsonTrue (Get-RequiredProperty $manifest "system_sleep_inhibited") "Manifest system_sleep_inhibited"
    Assert-JsonTrue (Get-RequiredProperty $manifest "system_sleep_inhibition_released") "Manifest system_sleep_inhibition_released"
    $releaseEvidence = [string](Get-RequiredProperty $manifest "sleep_inhibition_release_evidence")
    if ($finalizationMode -eq "supervised") {
        Assert-True ($releaseEvidence -eq "explicit_es_continuous") "Supervised sleep-inhibition release evidence is invalid."
    }
    else {
        Assert-True ($releaseEvidence -eq "supervisor_thread_terminated") "Recovered sleep-inhibition release evidence is invalid."
    }
    $finalizedUtc = [DateTimeOffset]::MinValue
    Assert-True ([DateTimeOffset]::TryParse([string](Get-RequiredProperty $manifest "finalized_utc"), [ref]$finalizedUtc)) "Manifest finalized_utc is invalid."
}
$sourceRevision = [string](Get-RequiredProperty $manifest "commit")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Manifest commit must be a full 40-character Git revision."
$expectedBinaryHash = [string](Get-RequiredProperty $manifest "binary_sha256")
Assert-True ($expectedBinaryHash -match '^[0-9a-fA-F]{64}$') "Manifest binary_sha256 must be a SHA-256 digest."

$binaryPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "binary_path")) $manifestDirectory
Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) "Endurance binary does not exist: $binaryPath"
$actualBinaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
Assert-True ($actualBinaryHash -ieq $expectedBinaryHash) "Endurance binary SHA-256 does not match the manifest."

if ($manifestSchema -eq 2) {
    foreach ($scriptName in @("supervisor", "finalizer", "verifier")) {
        $scriptPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "${scriptName}_script_path")) $manifestDirectory
        Assert-True (Test-Path -LiteralPath $scriptPath -PathType Leaf) "Captured desktop-endurance $scriptName script does not exist: $scriptPath"
        $expectedScriptHash = [string](Get-RequiredProperty $manifest "${scriptName}_script_sha256")
        Assert-True ($expectedScriptHash -match '^[0-9a-fA-F]{64}$') "Manifest ${scriptName}_script_sha256 must be a SHA-256 digest."
        $actualScriptHash = (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash
        Assert-True ($actualScriptHash -ieq $expectedScriptHash) "Captured desktop-endurance $scriptName script SHA-256 does not match the manifest."
    }
    $cargoLockPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "cargo_lock_path")) $manifestDirectory
    Assert-True (Test-Path -LiteralPath $cargoLockPath -PathType Leaf) "Manifest Cargo.lock does not exist: $cargoLockPath"
    $expectedCargoLockHash = [string](Get-RequiredProperty $manifest "cargo_lock_sha256")
    Assert-True ($expectedCargoLockHash -match '^[0-9a-fA-F]{64}$') "Manifest cargo_lock_sha256 must be a SHA-256 digest."
    $actualCargoLockHash = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
    Assert-True ($actualCargoLockHash -ieq $expectedCargoLockHash) "Cargo.lock SHA-256 does not match the manifest."
    foreach ($stream in @("stdout", "stderr")) {
        $streamPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "${stream}_path")) $manifestDirectory
        Assert-True (Test-Path -LiteralPath $streamPath -PathType Leaf) "Desktop endurance $stream log does not exist: $streamPath"
        $expectedStreamHash = [string](Get-RequiredProperty $manifest "${stream}_sha256")
        Assert-True ($expectedStreamHash -match '^[0-9a-fA-F]{64}$') "Manifest ${stream}_sha256 must be a SHA-256 digest."
        $actualStreamHash = (Get-FileHash -LiteralPath $streamPath -Algorithm SHA256).Hash
        Assert-True ($actualStreamHash -ieq $expectedStreamHash) "Desktop endurance $stream log SHA-256 does not match the manifest."
    }
}

$requestedDuration = Get-RequiredProperty $manifest "requested_duration_seconds"
Assert-UnsignedIntegerAtLeast $requestedDuration 28800 "Manifest requested_duration_seconds"
Assert-True ($requestedDuration -eq 28800) "Manifest must request exactly 28,800 seconds."
$pidValue = Get-RequiredProperty $manifest "pid"
Assert-UnsignedIntegerAtLeast $pidValue 1 "Manifest pid"
if ([uint64]$pidValue -le [int]::MaxValue) {
    $runningProcess = Get-Process -Id ([int]$pidValue) -ErrorAction SilentlyContinue
    Assert-True ($null -eq $runningProcess) "Endurance process $pidValue is still running; final evidence cannot be verified yet."
}

$startedUtc = [DateTimeOffset]::MinValue
Assert-True ([DateTimeOffset]::TryParse([string](Get-RequiredProperty $manifest "started_utc"), [ref]$startedUtc)) "Manifest started_utc is invalid."
if ($manifestSchema -eq 2) {
    Assert-True ($finalizedUtc -ge $startedUtc) "Manifest finalized_utc predates started_utc."
}
$reportPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "report_path")) $manifestDirectory
$expectedReportHash = [string](Get-RequiredProperty $manifest "report_sha256")
Assert-True ($expectedReportHash -match '^[0-9a-fA-F]{64}$') "Manifest report_sha256 must be a SHA-256 digest added after the endurance process exits."
$report = Read-JsonArtifact $reportPath "Desktop endurance report"
$actualReportHash = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
Assert-True ($actualReportHash -ieq $expectedReportHash) "Desktop endurance report SHA-256 does not match the final manifest."

Assert-True ((Get-RequiredProperty $report "schema_version") -eq 2) "Desktop endurance report schema_version must be 2."
Assert-True ((Get-RequiredProperty $report "evidence_scope") -eq "synthetic_headless_desktop_continuous_endurance") "Desktop endurance report has the wrong evidence_scope."
Assert-True ((Get-RequiredProperty $report "completion_state") -eq "completed") "Desktop endurance report is not completed."
$reportRequestedDuration = Get-RequiredProperty $report "requested_duration_seconds"
$reportRequiredDuration = Get-RequiredProperty $report "required_qualification_duration_seconds"
Assert-UnsignedIntegerAtLeast $reportRequestedDuration 28800 "Report requested_duration_seconds"
Assert-UnsignedIntegerAtLeast $reportRequiredDuration 28800 "Report required_qualification_duration_seconds"
Assert-True ($reportRequestedDuration -eq 28800) "Report must request exactly 28,800 seconds."
Assert-True ($reportRequiredDuration -eq 28800) "Report qualification duration must be exactly 28,800 seconds."
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "elapsed_milliseconds") 28800000 "Report elapsed_milliseconds"
$minimumCheckpointSequence = if ($manifestSchema -eq 2) { 480 } else { 1 }
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "checkpoint_sequence") $minimumCheckpointSequence "Report checkpoint_sequence"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "frame_cycles") 1 "Report frame_cycles"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "updates_published") 1 "Report updates_published"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "last_generation") 1 "Report last_generation"
Assert-True ((Get-RequiredProperty $report "last_generation") -eq (Get-RequiredProperty $report "updates_published")) "Report generation and publication counts diverged."
Assert-True ((Get-RequiredProperty $report "stale_or_gapped_publications") -eq 0) "Report contains stale or gapped publications."
Assert-JsonTrue (Get-RequiredProperty $report "working_set_within_bound") "Report working_set_within_bound"
Assert-JsonTrue (Get-RequiredProperty $report "clean_stop") "Report clean_stop"
Assert-JsonTrue (Get-RequiredProperty $report "synthetic_bounds_qualified") "Report synthetic_bounds_qualified"
$liveMarketGate = [string](Get-RequiredProperty $report "live_market_gate")
Assert-True ($liveMarketGate -eq "not_run" -or $liveMarketGate -eq "passed" -or $liveMarketGate -eq "failed") "Report live_market_gate is invalid."

$mailboxCapacityValue = Get-RequiredProperty $report "mailbox_capacity"
$mailboxHighWaterValue = Get-RequiredProperty $report "mailbox_high_water_items"
Assert-UnsignedIntegerAtLeast $mailboxCapacityValue 1 "Report mailbox_capacity"
Assert-UnsignedIntegerAtLeast $mailboxHighWaterValue 0 "Report mailbox_high_water_items"
$mailboxCapacity = [uint64]$mailboxCapacityValue
$mailboxHighWater = [uint64]$mailboxHighWaterValue
Assert-True ($mailboxCapacity -gt 0 -and $mailboxHighWater -le $mailboxCapacity) "Report mailbox high water exceeds capacity."
$baselineBytesValue = Get-RequiredProperty $report "working_set_baseline_bytes"
$highWaterBytesValue = Get-RequiredProperty $report "working_set_sampled_high_water_bytes"
$maximumGrowthBytesValue = Get-RequiredProperty $report "maximum_working_set_growth_bytes"
Assert-UnsignedIntegerAtLeast $baselineBytesValue 1 "Report working_set_baseline_bytes"
Assert-UnsignedIntegerAtLeast $highWaterBytesValue 1 "Report working_set_sampled_high_water_bytes"
Assert-UnsignedIntegerAtLeast $maximumGrowthBytesValue 1 "Report maximum_working_set_growth_bytes"
$baselineBytes = [uint64]$baselineBytesValue
$highWaterBytes = [uint64]$highWaterBytesValue
$maximumGrowthBytes = [uint64]$maximumGrowthBytesValue
Assert-True ($highWaterBytes -ge $baselineBytes) "Report working-set high water is below its baseline."
Assert-True (($highWaterBytes - $baselineBytes) -le $maximumGrowthBytes) "Report working-set growth exceeds its declared maximum."

$checkpointMilliseconds = [uint64](Get-RequiredProperty $report "checkpoint_unix_milliseconds")
$startedMilliseconds = [uint64]$startedUtc.ToUnixTimeMilliseconds()
Assert-True ($checkpointMilliseconds -ge $startedMilliseconds) "Report checkpoint predates the manifest start time."
$wallElapsedMilliseconds = $checkpointMilliseconds - $startedMilliseconds
$reportedElapsedMilliseconds = [uint64](Get-RequiredProperty $report "elapsed_milliseconds")
$maximumClockToleranceMilliseconds = 60000L
Assert-True ($wallElapsedMilliseconds -ge 28800000) "Manifest-to-checkpoint wall time did not reach eight hours."
$elapsedDifference = [Math]::Abs([decimal]$wallElapsedMilliseconds - [decimal]$reportedElapsedMilliseconds)
Assert-True ($elapsedDifference -le $maximumClockToleranceMilliseconds) "Manifest-to-checkpoint wall time differs from monotonic elapsed time by more than 60 seconds."

Write-Output "desktop_endurance_evidence=verified schema=2 source_revision=$sourceRevision binary_sha256=$($actualBinaryHash.ToUpperInvariant()) report_sha256=$($actualReportHash.ToUpperInvariant()) duration_seconds=28800 report=$reportPath"
