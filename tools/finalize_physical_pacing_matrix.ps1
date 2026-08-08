<#
.SYNOPSIS
Pins external 60/120/144 Hz evidence hashes into a matrix manifest and verifies the complete gate.

.DESCRIPTION
Each profile must first have diagnostics-N.json from run_physical_pacing_profile.ps1 and an external-N.json
created from the prepared template using the external scanout instrument's real measurements.

.EXAMPLE
powershell -File tools/finalize_physical_pacing_matrix.ps1 -SessionPath local-data/evidence/physical-pacing/capture-session.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SessionPath
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

function Write-Json {
    param([string]$Path, [object]$Value)
    $json = $Value | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText($Path, $json + [Environment]::NewLine)
}

$resolvedSessionPath = [IO.Path]::GetFullPath($SessionPath)
Assert-True (Test-Path -LiteralPath $resolvedSessionPath -PathType Leaf) "Capture session does not exist: $resolvedSessionPath"
$sessionDirectory = Split-Path -Parent $resolvedSessionPath
$session = Get-Content -LiteralPath $resolvedSessionPath -Raw | ConvertFrom-Json
Assert-True ((Get-RequiredProperty $session "schema_version") -eq 1) "Capture session schema_version must be 1."
Assert-True ((Get-RequiredProperty $session "evidence_scope") -eq "external_physical_scanout_pacing_preparation") "Capture session has the wrong evidence_scope."
Assert-True ((Get-RequiredProperty $session "preparation_state") -eq "prepared") "Capture session is not prepared."
$sourceRevision = [string](Get-RequiredProperty $session "source_revision")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Capture session source_revision must be a full Git revision."
$binaryHash = [string](Get-RequiredProperty $session "binary_sha256")
Assert-True ($binaryHash -match '^[0-9a-fA-F]{64}$') "Capture session binary_sha256 must be a SHA-256 digest."
$binaryPath = [IO.Path]::GetFullPath((Join-Path $sessionDirectory ([string](Get-RequiredProperty $session "binary_path"))))
Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) "Frozen desktop executable is missing: $binaryPath"
Assert-True ((Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash -ieq $binaryHash) "Frozen desktop executable hash does not match the capture session."

$matrixPath = Join-Path $sessionDirectory "matrix.json"
Assert-True (-not (Test-Path -LiteralPath $matrixPath)) "Matrix manifest already exists; refusing to overwrite finalized evidence: $matrixPath"
$profiles = @()
foreach ($target in @(60, 120, 144)) {
    $diagnosticsPath = Join-Path $sessionDirectory ("diagnostics-{0}.json" -f $target)
    $evidencePath = Join-Path $sessionDirectory ("external-{0}.json" -f $target)
    Assert-True (Test-Path -LiteralPath $diagnosticsPath -PathType Leaf) "$target Hz desktop diagnostics are missing: $diagnosticsPath"
    Assert-True (Test-Path -LiteralPath $evidencePath -PathType Leaf) "$target Hz external scanout evidence is missing: $evidencePath"

    $diagnostics = Get-Content -LiteralPath $diagnosticsPath -Raw | ConvertFrom-Json
    Assert-True ((Get-RequiredProperty $diagnostics "schema_version") -eq 3) "$target Hz desktop diagnostics schema_version must be 3."
    Assert-True ((Get-RequiredProperty $diagnostics "evidence_scope") -eq "windowed_replay_to_frame_callback_and_native_compositor_timeline") "$target Hz desktop diagnostics have the wrong evidence_scope."
    Assert-True (([string](Get-RequiredProperty $diagnostics "source_revision")) -eq $sourceRevision) "$target Hz desktop diagnostics source revision does not match the session."
    Assert-True ((Get-RequiredProperty $diagnostics "physical_presentation_measured") -is [bool] -and -not (Get-RequiredProperty $diagnostics "physical_presentation_measured")) "$target Hz desktop diagnostics must remain honestly labeled as non-physical supporting evidence."
    Assert-True ((Get-RequiredProperty $diagnostics "external_scanout_instrumented") -is [bool] -and -not (Get-RequiredProperty $diagnostics "external_scanout_instrumented")) "$target Hz desktop diagnostics must remain honestly labeled as non-external supporting evidence."
    $display = Get-RequiredProperty $diagnostics "display"
    $outputs = @((Get-RequiredProperty $display "outputs"))
    $targetMillihertz = [int64]$target * 1000
    $tolerance = [int64][Math]::Ceiling($targetMillihertz * 0.01)
    $matchingOutput = @($outputs | Where-Object {
        $refresh = $_.PSObject.Properties["refresh_millihertz"]
        $null -ne $refresh -and $null -ne $refresh.Value -and [Math]::Abs([int64]$refresh.Value - $targetMillihertz) -le $tolerance
    })
    Assert-True ($matchingOutput.Count -gt 0) "$target Hz desktop diagnostics do not name an observed output within one percent of the target refresh."

    $profiles += [ordered]@{
        target_refresh_hz = $target
        evidence_path = [IO.Path]::GetFileName($evidencePath)
        evidence_sha256 = (Get-FileHash -LiteralPath $evidencePath -Algorithm SHA256).Hash.ToUpperInvariant()
        supporting_diagnostics_path = [IO.Path]::GetFileName($diagnosticsPath)
        supporting_diagnostics_sha256 = (Get-FileHash -LiteralPath $diagnosticsPath -Algorithm SHA256).Hash.ToUpperInvariant()
    }
}

$manifest = [ordered]@{
    schema_version = 1
    evidence_scope = "external_physical_scanout_pacing_matrix"
    source_revision = $sourceRevision
    binary_path = [IO.Path]::GetFileName($binaryPath)
    binary_sha256 = $binaryHash.ToUpperInvariant()
    capture_session_path = [IO.Path]::GetFileName($resolvedSessionPath)
    capture_session_sha256 = (Get-FileHash -LiteralPath $resolvedSessionPath -Algorithm SHA256).Hash.ToUpperInvariant()
    profiles = $profiles
}
Write-Json $matrixPath $manifest

$verifier = Join-Path $PSScriptRoot "verify_physical_pacing_matrix.ps1"
try {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $verifier -ManifestPath $matrixPath
    Assert-True ($LASTEXITCODE -eq 0) "Physical pacing matrix verification failed."
}
catch {
    if (Test-Path -LiteralPath $matrixPath) {
        Remove-Item -LiteralPath $matrixPath -Force
    }
    throw
}

Write-Output "physical_pacing_capture=finalized manifest=$matrixPath"
