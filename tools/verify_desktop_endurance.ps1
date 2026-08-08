<#
.SYNOPSIS
Verifies a completed eight-hour desktop-endurance artifact without controlling its process.

.DESCRIPTION
The schema-1 manifest must contain commit, binary_sha256, binary_path, report_path,
report_sha256, pid, requested_duration_seconds, and started_utc. The report hash is
added to the manifest only after the process exits. Relative artifact paths resolve
from the manifest directory. Verification fails closed while the recorded PID is
running and never stops, signals, or otherwise changes that process.

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

Assert-True ((Get-RequiredProperty $manifest "schema_version") -eq 1) "Desktop endurance manifest schema_version must be 1."
$sourceRevision = [string](Get-RequiredProperty $manifest "commit")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Manifest commit must be a full 40-character Git revision."
$expectedBinaryHash = [string](Get-RequiredProperty $manifest "binary_sha256")
Assert-True ($expectedBinaryHash -match '^[0-9a-fA-F]{64}$') "Manifest binary_sha256 must be a SHA-256 digest."

$binaryPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "binary_path")) $manifestDirectory
Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) "Endurance binary does not exist: $binaryPath"
$actualBinaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
Assert-True ($actualBinaryHash -ieq $expectedBinaryHash) "Endurance binary SHA-256 does not match the manifest."

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
$reportPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "report_path")) $manifestDirectory
$expectedReportHash = [string](Get-RequiredProperty $manifest "report_sha256")
Assert-True ($expectedReportHash -match '^[0-9a-fA-F]{64}$') "Manifest report_sha256 must be a SHA-256 digest added after the endurance process exits."
$report = Read-JsonArtifact $reportPath "Desktop endurance report"
$actualReportHash = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
Assert-True ($actualReportHash -ieq $expectedReportHash) "Desktop endurance report SHA-256 does not match the final manifest."

Assert-True ((Get-RequiredProperty $report "schema_version") -eq 2) "Desktop endurance report schema_version must be 2."
Assert-True ((Get-RequiredProperty $report "evidence_scope") -eq "headless_desktop_continuous_endurance") "Desktop endurance report has the wrong evidence_scope."
Assert-True ((Get-RequiredProperty $report "completion_state") -eq "completed") "Desktop endurance report is not completed."
$reportRequestedDuration = Get-RequiredProperty $report "requested_duration_seconds"
$reportRequiredDuration = Get-RequiredProperty $report "required_qualification_duration_seconds"
Assert-UnsignedIntegerAtLeast $reportRequestedDuration 28800 "Report requested_duration_seconds"
Assert-UnsignedIntegerAtLeast $reportRequiredDuration 28800 "Report required_qualification_duration_seconds"
Assert-True ($reportRequestedDuration -eq 28800) "Report must request exactly 28,800 seconds."
Assert-True ($reportRequiredDuration -eq 28800) "Report qualification duration must be exactly 28,800 seconds."
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "elapsed_milliseconds") 28800000 "Report elapsed_milliseconds"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "checkpoint_sequence") 1 "Report checkpoint_sequence"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "frame_cycles") 1 "Report frame_cycles"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "updates_published") 1 "Report updates_published"
Assert-UnsignedIntegerAtLeast (Get-RequiredProperty $report "last_generation") 1 "Report last_generation"
Assert-True ((Get-RequiredProperty $report "last_generation") -eq (Get-RequiredProperty $report "updates_published")) "Report generation and publication counts diverged."
Assert-True ((Get-RequiredProperty $report "stale_or_gapped_publications") -eq 0) "Report contains stale or gapped publications."
Assert-JsonTrue (Get-RequiredProperty $report "working_set_within_bound") "Report working_set_within_bound"
Assert-JsonTrue (Get-RequiredProperty $report "clean_stop") "Report clean_stop"
Assert-JsonTrue (Get-RequiredProperty $report "readiness_qualified") "Report readiness_qualified"

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
