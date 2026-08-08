<#
.SYNOPSIS
Runs one exact frozen desktop benchmark profile for simultaneous external scanout instrumentation.

.DESCRIPTION
Set the named display to the requested refresh rate and start the external instrument before invoking this script.
The generated diagnostics file is compositor evidence only; it is retained to prove which configured display mode
the exact frozen binary observed while the external instrument collected physical scanout samples.

.EXAMPLE
powershell -File tools/run_physical_pacing_profile.ps1 -SessionPath local-data/evidence/physical-pacing/capture-session.json -TargetRefreshHz 60
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$SessionPath,

    [Parameter(Mandatory = $true)]
    [ValidateSet(60, 120, 144)]
    [int]$TargetRefreshHz
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

function Get-GitText {
    param([string[]]$Arguments)
    $output = & git @Arguments 2>&1
    Assert-True ($LASTEXITCODE -eq 0) "git $($Arguments -join ' ') failed: $output"
    return ([string]($output -join "`n")).Trim()
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$resolvedSessionPath = [IO.Path]::GetFullPath($SessionPath)
Assert-True (Test-Path -LiteralPath $resolvedSessionPath -PathType Leaf) "Capture session does not exist: $resolvedSessionPath"
$sessionDirectory = Split-Path -Parent $resolvedSessionPath
$session = Get-Content -LiteralPath $resolvedSessionPath -Raw | ConvertFrom-Json
Assert-True ((Get-RequiredProperty $session "schema_version") -eq 1) "Capture session schema_version must be 1."
Assert-True ((Get-RequiredProperty $session "evidence_scope") -eq "external_physical_scanout_pacing_preparation") "Capture session has the wrong evidence_scope."
Assert-True ((Get-RequiredProperty $session "preparation_state") -eq "prepared") "Capture session is not prepared."
$sourceRevision = [string](Get-RequiredProperty $session "source_revision")
$expectedBinaryHash = [string](Get-RequiredProperty $session "binary_sha256")
$expectedCargoLockHash = [string](Get-RequiredProperty $session "cargo_lock_sha256")
$binaryPath = [IO.Path]::GetFullPath((Join-Path $sessionDirectory ([string](Get-RequiredProperty $session "binary_path"))))
$frozenCargoLockPath = [IO.Path]::GetFullPath((Join-Path $sessionDirectory ([string](Get-RequiredProperty $session "cargo_lock_path"))))
Assert-True (Test-Path -LiteralPath $binaryPath -PathType Leaf) "Frozen desktop executable is missing: $binaryPath"
Assert-True ((Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash -ieq $expectedBinaryHash) "Frozen desktop executable hash does not match the capture session."
Assert-True (Test-Path -LiteralPath $frozenCargoLockPath -PathType Leaf) "Frozen Cargo.lock is missing: $frozenCargoLockPath"
Assert-True ((Get-FileHash -LiteralPath $frozenCargoLockPath -Algorithm SHA256).Hash -ieq $expectedCargoLockHash) "Frozen Cargo.lock hash does not match the capture session."

$diagnosticsPath = Join-Path $sessionDirectory ("diagnostics-{0}.json" -f $TargetRefreshHz)
$incompleteDiagnosticsPath = Join-Path $sessionDirectory ("diagnostics-{0}.incomplete.json" -f $TargetRefreshHz)
Assert-True (-not (Test-Path -LiteralPath $diagnosticsPath)) "Profile diagnostics already exist; refusing to overwrite a capture: $diagnosticsPath"
Assert-True (-not (Test-Path -LiteralPath $incompleteDiagnosticsPath)) "An incomplete profile diagnostics file already exists: $incompleteDiagnosticsPath"

Push-Location $repositoryRoot
$previousGithubSha = [Environment]::GetEnvironmentVariable("GITHUB_SHA", "Process")
try {
    Assert-True ((Get-GitText @("rev-parse", "HEAD")) -eq $sourceRevision) "Current source revision does not match the prepared capture session."
    Assert-True ([string]::IsNullOrWhiteSpace((Get-GitText @("status", "--porcelain")))) "Physical pacing capture requires a clean worktree."
    Assert-True ((Get-FileHash -LiteralPath (Join-Path $repositoryRoot "Cargo.lock") -Algorithm SHA256).Hash -ieq $expectedCargoLockHash) "Cargo.lock changed after capture preparation."

    [Environment]::SetEnvironmentVariable("GITHUB_SHA", $sourceRevision, "Process")
    & $binaryPath --windowed-benchmark $incompleteDiagnosticsPath
    Assert-True ($LASTEXITCODE -eq 0) "Frozen desktop benchmark exited unsuccessfully."
    Assert-True (Test-Path -LiteralPath $incompleteDiagnosticsPath -PathType Leaf) "Frozen desktop benchmark did not write diagnostics."

    Assert-True ((Get-GitText @("rev-parse", "HEAD")) -eq $sourceRevision) "Source revision changed during capture."
    Assert-True ([string]::IsNullOrWhiteSpace((Get-GitText @("status", "--porcelain")))) "The worktree changed during capture."
    Assert-True ((Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash -ieq $expectedBinaryHash) "Frozen desktop executable changed during capture."
    Assert-True ((Get-FileHash -LiteralPath $frozenCargoLockPath -Algorithm SHA256).Hash -ieq $expectedCargoLockHash) "Frozen Cargo.lock changed during capture."
    Assert-True ((Get-FileHash -LiteralPath (Join-Path $repositoryRoot "Cargo.lock") -Algorithm SHA256).Hash -ieq $expectedCargoLockHash) "Cargo.lock changed during capture."

    $diagnostics = Get-Content -LiteralPath $incompleteDiagnosticsPath -Raw | ConvertFrom-Json
    Assert-True ((Get-RequiredProperty $diagnostics "schema_version") -eq 3) "Desktop diagnostics schema_version must be 3."
    Assert-True ((Get-RequiredProperty $diagnostics "evidence_scope") -eq "windowed_replay_to_frame_callback_and_native_compositor_timeline") "Desktop diagnostics have the wrong evidence_scope."
    Assert-True (([string](Get-RequiredProperty $diagnostics "source_revision")) -eq $sourceRevision) "Desktop diagnostics source revision does not match the session."
    $outputs = @((Get-RequiredProperty (Get-RequiredProperty $diagnostics "display") "outputs"))
    $targetMillihertz = [int64]$TargetRefreshHz * 1000
    $tolerance = [int64][Math]::Ceiling($targetMillihertz * 0.01)
    $matchingOutput = @($outputs | Where-Object {
        $refresh = $_.PSObject.Properties["refresh_millihertz"]
        $null -ne $refresh -and $null -ne $refresh.Value -and [Math]::Abs([int64]$refresh.Value - $targetMillihertz) -le $tolerance
    })
    Assert-True ($matchingOutput.Count -gt 0) "No observed display output matched $TargetRefreshHz Hz within one percent. Set the display mode and capture this profile again."

    Move-Item -LiteralPath $incompleteDiagnosticsPath -Destination $diagnosticsPath
    $diagnosticsHash = (Get-FileHash -LiteralPath $diagnosticsPath -Algorithm SHA256).Hash.ToUpperInvariant()
    Write-Output "physical_pacing_profile=captured target_hz=$TargetRefreshHz diagnostics=$diagnosticsPath diagnostics_sha256=$diagnosticsHash"
}
finally {
    [Environment]::SetEnvironmentVariable("GITHUB_SHA", $previousGithubSha, "Process")
    if (Test-Path -LiteralPath $incompleteDiagnosticsPath) {
        Remove-Item -LiteralPath $incompleteDiagnosticsPath -Force
    }
    Pop-Location
}
