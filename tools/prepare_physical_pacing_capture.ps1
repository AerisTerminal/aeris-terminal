<#
.SYNOPSIS
Builds and freezes the exact desktop executable for the 60/120/144 Hz physical-scanout capture matrix.

.EXAMPLE
powershell -File tools/prepare_physical_pacing_capture.ps1 -OutputDirectory local-data/evidence/physical-pacing
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Get-GitText {
    param([string[]]$Arguments)
    $output = & git @Arguments 2>&1
    Assert-True ($LASTEXITCODE -eq 0) "git $($Arguments -join ' ') failed: $output"
    return ([string]($output -join "`n")).Trim()
}

function Write-Json {
    param([string]$Path, [object]$Value)
    $json = $Value | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText($Path, $json + [Environment]::NewLine)
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$resolvedOutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
Assert-True (-not (Test-Path -LiteralPath $resolvedOutputDirectory)) "Output directory already exists; refusing to overwrite capture state: $resolvedOutputDirectory"

Push-Location $repositoryRoot
try {
    $sourceRevision = Get-GitText @("rev-parse", "HEAD")
    Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Git did not return a full source revision."
    Assert-True ([string]::IsNullOrWhiteSpace((Get-GitText @("status", "--porcelain")))) "Physical pacing preparation requires a clean worktree."

    & cargo build --locked --release -p asceify_desktop --bin asceify_desktop
    Assert-True ($LASTEXITCODE -eq 0) "Release desktop build failed."

    Assert-True ((Get-GitText @("rev-parse", "HEAD")) -eq $sourceRevision) "Source revision changed during the release build."
    Assert-True ([string]::IsNullOrWhiteSpace((Get-GitText @("status", "--porcelain")))) "The worktree changed during the release build."

    $builtExecutable = Join-Path $repositoryRoot "target\release\asceify_desktop.exe"
    Assert-True (Test-Path -LiteralPath $builtExecutable -PathType Leaf) "Release desktop executable was not produced: $builtExecutable"
    $cargoLockPath = Join-Path $repositoryRoot "Cargo.lock"
    Assert-True (Test-Path -LiteralPath $cargoLockPath -PathType Leaf) "Cargo.lock is missing."

    $null = New-Item -ItemType Directory -Path $resolvedOutputDirectory
    $frozenExecutable = Join-Path $resolvedOutputDirectory "asceify_desktop.exe"
    $frozenCargoLock = Join-Path $resolvedOutputDirectory "Cargo.lock"
    Copy-Item -LiteralPath $builtExecutable -Destination $frozenExecutable
    Copy-Item -LiteralPath $cargoLockPath -Destination $frozenCargoLock
    $binaryHash = (Get-FileHash -LiteralPath $frozenExecutable -Algorithm SHA256).Hash.ToUpperInvariant()
    $cargoLockHash = (Get-FileHash -LiteralPath $frozenCargoLock -Algorithm SHA256).Hash.ToUpperInvariant()

    $session = [ordered]@{
        schema_version = 1
        evidence_scope = "external_physical_scanout_pacing_preparation"
        preparation_state = "prepared"
        source_revision = $sourceRevision
        source_worktree_clean = $true
        cargo_lock_path = "Cargo.lock"
        cargo_lock_sha256 = $cargoLockHash
        binary_path = "asceify_desktop.exe"
        binary_sha256 = $binaryHash
        required_profiles_hz = @(60, 120, 144)
        prepared_utc = [DateTimeOffset]::UtcNow.ToString("O")
    }
    $sessionPath = Join-Path $resolvedOutputDirectory "capture-session.json"
    Write-Json $sessionPath $session

    foreach ($target in @(60, 120, 144)) {
        $template = [ordered]@{
            schema_version = 1
            evidence_scope = "external_physical_scanout_pacing"
            source_revision = $sourceRevision
            binary_sha256 = $binaryHash
            target_refresh_hz = $target
            configured_refresh_millihertz = $null
            measured_refresh_millihertz = $null
            physical_presentation_measured = $false
            external_scanout_instrumented = $false
            measurement_method = ""
            instrument_name = ""
            capture_id = ""
            display_name = ""
            display_device_path = ""
            warmup_frames = 32
            sample_count = 0
            frame_interval_nanos = [ordered]@{
                p50 = $null
                p95 = $null
                p99 = $null
                p99_9 = $null
                maximum = $null
            }
            loss_recovery_counters = [ordered]@{
                late_frames = $null
                dropped_frames = $null
                missed_frames = $null
                recovery_events = $null
            }
        }
        Write-Json (Join-Path $resolvedOutputDirectory ("external-{0}.template.json" -f $target)) $template
    }

    Write-Output "physical_pacing_capture=prepared session=$sessionPath source_revision=$sourceRevision binary_sha256=$binaryHash"
}
finally {
    Pop-Location
}
