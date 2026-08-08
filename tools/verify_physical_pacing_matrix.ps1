<#
.SYNOPSIS
Verifies an externally instrumented physical-scanout matrix at 60, 120, and 144 Hz.

.DESCRIPTION
The schema-1 matrix manifest names the measured source revision, binary path and
SHA-256, and exactly three profile objects containing target_refresh_hz,
evidence_path, and evidence_sha256. Each schema-1 evidence artifact must declare
external_physical_scanout_pacing, physical/external measurement booleans, matching
source and binary provenance, named instrument/display/capture identity, configured
and measured refresh, warmup/sample counts, frame-interval percentiles, and
late/dropped/missed/recovery counters. DWM, GPUI callback, and compositor-only
reports are supporting diagnostics and are deliberately rejected by this gate.

.EXAMPLE
powershell -File tools/verify_physical_pacing_matrix.ps1 -ManifestPath local-data/evidence/pacing/matrix.json
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

function Assert-NonEmptyString {
    param([object]$Value, [string]$Name)
    Assert-True ($Value -is [string] -and -not [string]::IsNullOrWhiteSpace($Value)) "$Name must be a non-empty string."
}

function Assert-NonnegativeInteger {
    param([object]$Value, [string]$Name)
    $converted = 0L
    Assert-True ($Value -isnot [string] -and $Value -isnot [bool] -and [Int64]::TryParse([string]$Value, [ref]$converted) -and $converted -ge 0) "$Name must be a nonnegative JSON integer."
}

function Assert-JsonTrue {
    param([object]$Value, [string]$Name)
    Assert-True ($Value -is [bool] -and $Value) "$Name must be the JSON boolean true."
}

$resolvedManifestPath = [IO.Path]::GetFullPath($ManifestPath)
$manifestDirectory = Split-Path -Parent $resolvedManifestPath
$manifest = Read-JsonArtifact $resolvedManifestPath "Physical-pacing manifest"

Assert-True ((Get-RequiredProperty $manifest "schema_version") -eq 1) "Physical-pacing manifest schema_version must be 1."
Assert-True ((Get-RequiredProperty $manifest "evidence_scope") -eq "external_physical_scanout_pacing_matrix") "Physical-pacing manifest has the wrong evidence_scope."
$sourceRevision = [string](Get-RequiredProperty $manifest "source_revision")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Manifest source_revision must be a full 40-character Git revision."
$binaryHash = [string](Get-RequiredProperty $manifest "binary_sha256")
Assert-True ($binaryHash -match '^[0-9a-fA-F]{64}$') "Manifest binary_sha256 must be a SHA-256 digest."
$binaryPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $manifest "binary_path")) $manifestDirectory
Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) "Measured binary does not exist: $binaryPath"
$actualBinaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
Assert-True ($actualBinaryHash -ieq $binaryHash) "Measured binary SHA-256 does not match the manifest."

$profiles = @(Get-RequiredProperty $manifest "profiles")
Assert-True ($profiles.Count -eq 3) "Physical-pacing manifest must contain exactly three profiles."
$requiredTargets = @(60, 120, 144)
$observedTargets = @($profiles | ForEach-Object { [int](Get-RequiredProperty $_ "target_refresh_hz") } | Sort-Object)
Assert-True (($observedTargets -join ',') -eq ($requiredTargets -join ',')) "Physical-pacing profiles must be exactly 60, 120, and 144 Hz."
$evidencePaths = @{}
$captureIds = @{}

foreach ($profile in $profiles) {
    $targetValue = Get-RequiredProperty $profile "target_refresh_hz"
    Assert-NonnegativeInteger $targetValue "Profile target_refresh_hz"
    $target = [int]$targetValue
    $evidencePath = Resolve-ArtifactPath ([string](Get-RequiredProperty $profile "evidence_path")) $manifestDirectory
    $expectedEvidenceHash = [string](Get-RequiredProperty $profile "evidence_sha256")
    Assert-True ($expectedEvidenceHash -match '^[0-9a-fA-F]{64}$') "$target Hz evidence_sha256 must be a SHA-256 digest."
    Assert-True (-not $evidencePaths.ContainsKey($evidencePath)) "Each refresh profile must use a distinct evidence artifact."
    $evidencePaths[$evidencePath] = $true
    $evidence = Read-JsonArtifact $evidencePath "$target Hz physical scanout evidence"
    $actualEvidenceHash = (Get-FileHash -LiteralPath $evidencePath -Algorithm SHA256).Hash
    Assert-True ($actualEvidenceHash -ieq $expectedEvidenceHash) "$target Hz evidence SHA-256 does not match the manifest."

    Assert-True ((Get-RequiredProperty $evidence "schema_version") -eq 1) "$target Hz evidence schema_version must be 1."
    Assert-True ((Get-RequiredProperty $evidence "evidence_scope") -eq "external_physical_scanout_pacing") "$target Hz report is not external physical scanout evidence; DWM or GPUI callback-only reports are rejected."
    Assert-JsonTrue (Get-RequiredProperty $evidence "physical_presentation_measured") "$target Hz physical_presentation_measured"
    Assert-JsonTrue (Get-RequiredProperty $evidence "external_scanout_instrumented") "$target Hz external_scanout_instrumented"
    Assert-True ((Get-RequiredProperty $evidence "target_refresh_hz") -eq $target) "$target Hz evidence target does not match its manifest profile."
    Assert-True (([string](Get-RequiredProperty $evidence "source_revision")) -ieq $sourceRevision) "$target Hz evidence source revision does not match the manifest."
    Assert-True (([string](Get-RequiredProperty $evidence "binary_sha256")) -ieq $binaryHash) "$target Hz evidence binary hash does not match the manifest."

    $method = [string](Get-RequiredProperty $evidence "measurement_method")
    Assert-NonEmptyString $method "$target Hz measurement_method"
    Assert-True ($method -notmatch '(?i)dwm|gpui|frame.?callback|compositor') "$target Hz measurement_method is compositor/callback-only, not external scanout instrumentation."
    Assert-NonEmptyString (Get-RequiredProperty $evidence "instrument_name") "$target Hz instrument_name"
    $captureId = Get-RequiredProperty $evidence "capture_id"
    Assert-NonEmptyString $captureId "$target Hz capture_id"
    Assert-True (-not $captureIds.ContainsKey([string]$captureId)) "Each refresh profile must use a distinct capture_id."
    $captureIds[[string]$captureId] = $true
    Assert-NonEmptyString (Get-RequiredProperty $evidence "display_name") "$target Hz display_name"
    Assert-NonEmptyString (Get-RequiredProperty $evidence "display_device_path") "$target Hz display_device_path"

    $configuredMillihertz = [uint64](Get-RequiredProperty $evidence "configured_refresh_millihertz")
    $measuredMillihertz = [uint64](Get-RequiredProperty $evidence "measured_refresh_millihertz")
    $targetMillihertz = [uint64]($target * 1000)
    $tolerance = [uint64]([Math]::Ceiling($targetMillihertz * 0.01))
    Assert-True ([Math]::Abs([int64]$configuredMillihertz - [int64]$targetMillihertz) -le [int64]$tolerance) "$target Hz configured refresh is outside the one-percent naming tolerance."
    Assert-True ([Math]::Abs([int64]$measuredMillihertz - [int64]$targetMillihertz) -le [int64]$tolerance) "$target Hz measured refresh is outside the one-percent naming tolerance."

    Assert-NonnegativeInteger (Get-RequiredProperty $evidence "warmup_frames") "$target Hz warmup_frames"
    $sampleCount = Get-RequiredProperty $evidence "sample_count"
    Assert-NonnegativeInteger $sampleCount "$target Hz sample_count"
    Assert-True ([uint64]$sampleCount -ge 256) "$target Hz evidence must include at least 256 measured frames."
    $intervals = Get-RequiredProperty $evidence "frame_interval_nanos"
    $percentiles = @()
    foreach ($name in @("p50", "p95", "p99", "p99_9", "maximum")) {
        $value = Get-RequiredProperty $intervals $name
        Assert-NonnegativeInteger $value "$target Hz frame_interval_nanos.$name"
        Assert-True ([uint64]$value -gt 0) "$target Hz frame_interval_nanos.$name must be positive."
        $percentiles += [uint64]$value
    }
    Assert-True ($percentiles[0] -le $percentiles[1] -and $percentiles[1] -le $percentiles[2] -and $percentiles[2] -le $percentiles[3] -and $percentiles[3] -le $percentiles[4]) "$target Hz frame-interval percentiles are not monotonic."
    $counters = Get-RequiredProperty $evidence "loss_recovery_counters"
    foreach ($name in @("late_frames", "dropped_frames", "missed_frames", "recovery_events")) {
        Assert-NonnegativeInteger (Get-RequiredProperty $counters $name) "$target Hz loss_recovery_counters.$name"
    }
}

Write-Output "physical_pacing_matrix=verified profiles=60,120,144 measurement=external_scanout source_revision=$sourceRevision binary_sha256=$($actualBinaryHash.ToUpperInvariant())"
