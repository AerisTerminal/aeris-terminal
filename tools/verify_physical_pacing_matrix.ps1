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

$captureSessionPathProperty = $manifest.PSObject.Properties["capture_session_path"]
$captureSessionHashProperty = $manifest.PSObject.Properties["capture_session_sha256"]
Assert-True (($null -eq $captureSessionPathProperty) -eq ($null -eq $captureSessionHashProperty)) "capture_session_path and capture_session_sha256 must either both be present or both be absent."
if ($null -ne $captureSessionPathProperty) {
    $captureSessionPath = Resolve-ArtifactPath ([string]$captureSessionPathProperty.Value) $manifestDirectory
    $captureSessionHash = [string]$captureSessionHashProperty.Value
    Assert-True ($captureSessionHash -match '^[0-9a-fA-F]{64}$') "capture_session_sha256 must be a SHA-256 digest."
    Assert-True (Test-Path -LiteralPath $captureSessionPath -PathType Leaf) "Capture session does not exist: $captureSessionPath"
    Assert-True ((Get-FileHash -LiteralPath $captureSessionPath -Algorithm SHA256).Hash -ieq $captureSessionHash) "Capture session SHA-256 does not match the manifest."
    $captureSession = Read-JsonArtifact $captureSessionPath "Physical-pacing capture session"
    Assert-True ((Get-RequiredProperty $captureSession "schema_version") -eq 1) "Capture session schema_version must be 1."
    Assert-True ((Get-RequiredProperty $captureSession "evidence_scope") -eq "external_physical_scanout_pacing_preparation") "Capture session has the wrong evidence_scope."
    Assert-True ((Get-RequiredProperty $captureSession "preparation_state") -eq "prepared") "Capture session is not prepared."
    Assert-JsonTrue (Get-RequiredProperty $captureSession "source_worktree_clean") "Capture session source_worktree_clean"
    Assert-True (([string](Get-RequiredProperty $captureSession "source_revision")) -ieq $sourceRevision) "Capture session source revision does not match the matrix."
    Assert-True (([string](Get-RequiredProperty $captureSession "binary_sha256")) -ieq $binaryHash) "Capture session binary hash does not match the matrix."
    $cargoLockHash = [string](Get-RequiredProperty $captureSession "cargo_lock_sha256")
    Assert-True ($cargoLockHash -match '^[0-9a-fA-F]{64}$') "Capture session cargo_lock_sha256 must be a SHA-256 digest."
    $cargoLockPath = Resolve-ArtifactPath ([string](Get-RequiredProperty $captureSession "cargo_lock_path")) (Split-Path -Parent $captureSessionPath)
    Assert-True (Test-Path -LiteralPath $cargoLockPath -PathType Leaf) "Capture session Cargo.lock does not exist: $cargoLockPath"
    Assert-True ((Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash -ieq $cargoLockHash) "Capture session Cargo.lock SHA-256 does not match."
    $requiredProfiles = @((Get-RequiredProperty $captureSession "required_profiles_hz") | ForEach-Object { [int]$_ } | Sort-Object)
    Assert-True (($requiredProfiles -join ',') -eq '60,120,144') "Capture session required_profiles_hz must be exactly 60, 120, and 144."
}

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

    $warmupFrames = Get-RequiredProperty $evidence "warmup_frames"
    Assert-NonnegativeInteger $warmupFrames "$target Hz warmup_frames"
    Assert-True ([uint64]$warmupFrames -ge 32) "$target Hz evidence must include at least 32 warmup frames."
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

    $supportingPathProperty = $profile.PSObject.Properties["supporting_diagnostics_path"]
    $supportingHashProperty = $profile.PSObject.Properties["supporting_diagnostics_sha256"]
    Assert-True (($null -eq $supportingPathProperty) -eq ($null -eq $supportingHashProperty)) "$target Hz supporting_diagnostics_path and supporting_diagnostics_sha256 must either both be present or both be absent."
    if ($null -ne $supportingPathProperty) {
        $supportingPath = Resolve-ArtifactPath ([string]$supportingPathProperty.Value) $manifestDirectory
        $supportingHash = [string]$supportingHashProperty.Value
        Assert-True ($supportingHash -match '^[0-9a-fA-F]{64}$') "$target Hz supporting_diagnostics_sha256 must be a SHA-256 digest."
        Assert-True (Test-Path -LiteralPath $supportingPath -PathType Leaf) "$target Hz supporting diagnostics do not exist: $supportingPath"
        Assert-True ((Get-FileHash -LiteralPath $supportingPath -Algorithm SHA256).Hash -ieq $supportingHash) "$target Hz supporting diagnostics SHA-256 does not match the manifest."
        $supporting = Read-JsonArtifact $supportingPath "$target Hz supporting diagnostics"
        Assert-True ((Get-RequiredProperty $supporting "schema_version") -eq 3) "$target Hz supporting diagnostics schema_version must be 3."
        Assert-True ((Get-RequiredProperty $supporting "evidence_scope") -eq "windowed_replay_to_frame_callback_and_native_compositor_timeline") "$target Hz supporting diagnostics have the wrong evidence_scope."
        Assert-True (([string](Get-RequiredProperty $supporting "source_revision")) -ieq $sourceRevision) "$target Hz supporting diagnostics source revision does not match the matrix."
        Assert-True ((Get-RequiredProperty $supporting "physical_presentation_measured") -is [bool] -and -not (Get-RequiredProperty $supporting "physical_presentation_measured")) "$target Hz supporting diagnostics must remain labeled as non-physical evidence."
        Assert-True ((Get-RequiredProperty $supporting "external_scanout_instrumented") -is [bool] -and -not (Get-RequiredProperty $supporting "external_scanout_instrumented")) "$target Hz supporting diagnostics must remain labeled as non-external evidence."
        $supportingOutputs = @((Get-RequiredProperty (Get-RequiredProperty $supporting "display") "outputs"))
        $matchingSupportingOutput = @($supportingOutputs | Where-Object {
            $refresh = $_.PSObject.Properties["refresh_millihertz"]
            $null -ne $refresh -and $null -ne $refresh.Value -and [Math]::Abs([int64]$refresh.Value - [int64]$targetMillihertz) -le [int64]$tolerance
        })
        Assert-True ($matchingSupportingOutput.Count -gt 0) "$target Hz supporting diagnostics do not name an observed output within one percent of the target refresh."
    }
}

Write-Output "physical_pacing_matrix=verified profiles=60,120,144 measurement=external_scanout source_revision=$sourceRevision binary_sha256=$($actualBinaryHash.ToUpperInvariant())"
