<#
.SYNOPSIS
Builds, runs, finalizes, and verifies one exact eight-hour desktop-endurance capture.

.DESCRIPTION
The launcher requires a clean worktree, copies the release executable into a new
evidence directory, and records source and binary provenance. With -Detached it
uses WMI to detach the frozen supervisor from the calling shell. This survives
the launcher or tool host exiting, but Windows still terminates it on user
logoff. It never reuses or deletes an evidence directory.

.EXAMPLE
powershell -File tools/run_desktop_endurance.ps1 -EvidenceDirectory local-data/evidence/desktop-endurance-20260808

powershell -File tools/run_desktop_endurance.ps1 -EvidenceDirectory local-data/evidence/desktop-endurance-20260808 -Detached
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$EvidenceDirectory,

    [switch]$Detached
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

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

function Get-CleanWorktree {
    param([string]$RepositoryRoot)
    $status = @(git -C $RepositoryRoot status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0) {
        throw "Could not inspect the Git worktree."
    }
    return $status.Count -eq 0
}

function Get-SourceRevision {
    param([string]$RepositoryRoot)
    $revision = (git -C $RepositoryRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $revision -notmatch '^[0-9a-fA-F]{40}$') {
        throw "Desktop endurance evidence requires a full source revision."
    }
    return $revision
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$resolvedEvidenceDirectory = [IO.Path]::GetFullPath($EvidenceDirectory)
if (Test-Path -LiteralPath $resolvedEvidenceDirectory) {
    throw "Desktop-endurance evidence directory must be new: $resolvedEvidenceDirectory"
}
if (-not (Get-CleanWorktree $repoRoot)) {
    throw "Desktop-endurance evidence requires a clean Git worktree."
}
$sourceRevision = Get-SourceRevision $repoRoot
$sourceCargoLockPath = (Resolve-Path -LiteralPath (Join-Path $repoRoot "Cargo.lock")).Path
$cargoLockHash = (Get-FileHash -LiteralPath $sourceCargoLockPath -Algorithm SHA256).Hash

cargo build --manifest-path (Join-Path $repoRoot "Cargo.toml") --locked --release --package aeris_desktop --all-features
if ($LASTEXITCODE -ne 0) {
    throw "Desktop-endurance release build failed."
}
if (-not (Get-CleanWorktree $repoRoot) -or (Get-SourceRevision $repoRoot) -ne $sourceRevision) {
    throw "Source provenance changed during the desktop-endurance build."
}
if ((Get-FileHash -LiteralPath $sourceCargoLockPath -Algorithm SHA256).Hash -ne $cargoLockHash) {
    throw "Cargo.lock changed during the desktop-endurance build."
}

$builtExecutable = (Resolve-Path -LiteralPath (Join-Path $repoRoot "target\release\aeris_desktop.exe")).Path
$null = New-Item -ItemType Directory -Path $resolvedEvidenceDirectory
$executablePath = Join-Path $resolvedEvidenceDirectory "aeris_desktop.exe"
$cargoLockPath = Join-Path $resolvedEvidenceDirectory "Cargo.lock"
$reportPath = Join-Path $resolvedEvidenceDirectory "desktop-endurance-schema2-8h.json"
$manifestPath = Join-Path $resolvedEvidenceDirectory "run-manifest.json"
$stdoutPath = Join-Path $resolvedEvidenceDirectory "stdout.log"
$stderrPath = Join-Path $resolvedEvidenceDirectory "stderr.log"
$supervisorPath = Join-Path $resolvedEvidenceDirectory "supervise_desktop_endurance.ps1"
$finalizerPath = Join-Path $resolvedEvidenceDirectory "finalize_desktop_endurance.ps1"
$verifierPath = Join-Path $resolvedEvidenceDirectory "verify_desktop_endurance.ps1"
Copy-Item -LiteralPath $builtExecutable -Destination $executablePath
Copy-Item -LiteralPath $sourceCargoLockPath -Destination $cargoLockPath
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "supervise_desktop_endurance.ps1") -Destination $supervisorPath
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "finalize_desktop_endurance.ps1") -Destination $finalizerPath
Copy-Item -LiteralPath (Join-Path $PSScriptRoot "verify_desktop_endurance.ps1") -Destination $verifierPath
$executableHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash

$manifest = [ordered]@{
    schema_version = 2
    evidence_scope = "desktop_endurance_capture_manifest"
    commit = $sourceRevision
    clean_worktree = $true
    cargo_lock_path = $cargoLockPath
    cargo_lock_sha256 = $cargoLockHash
    binary_path = $executablePath
    binary_sha256 = $executableHash
    supervisor_script_path = $supervisorPath
    supervisor_script_sha256 = (Get-FileHash -LiteralPath $supervisorPath -Algorithm SHA256).Hash
    finalizer_script_path = $finalizerPath
    finalizer_script_sha256 = (Get-FileHash -LiteralPath $finalizerPath -Algorithm SHA256).Hash
    verifier_script_path = $verifierPath
    verifier_script_sha256 = (Get-FileHash -LiteralPath $verifierPath -Algorithm SHA256).Hash
    report_path = $reportPath
    report_sha256 = $null
    stdout_path = $stdoutPath
    stdout_sha256 = $null
    stderr_path = $stderrPath
    stderr_sha256 = $null
    pid = $null
    supervisor_pid = $null
    supervisor_error = $null
    requested_duration_seconds = 28800
    capture_requested_utc = [DateTimeOffset]::UtcNow.ToString("O")
    started_utc = $null
    process_exit_code = $null
    process_exit_evidence = $null
    finalization_mode = $null
    system_sleep_inhibited = $false
    system_sleep_inhibition_released = $false
    sleep_inhibition_release_evidence = $null
    launch_mode = if ($Detached) { "wmi_detached_interactive_session" } else { "foreground_supervisor" }
    logoff_resilient = $false
    finalized = $false
    finalized_utc = $null
}
Write-JsonAtomically $manifestPath $manifest

$supervisor = $supervisorPath
if (-not $Detached) {
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $supervisor -ManifestPath $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Desktop-endurance supervisor failed. Inspect $stdoutPath and $stderrPath."
    }
    return
}

$quotedSupervisor = '"' + $supervisor + '"'
$quotedManifest = '"' + $manifestPath + '"'
$commandLine = "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $quotedSupervisor -ManifestPath $quotedManifest"
$created = Invoke-CimMethod `
    -ClassName Win32_Process `
    -MethodName Create `
    -Arguments @{ CommandLine = $commandLine; CurrentDirectory = $repoRoot }
if ($created.ReturnValue -ne 0 -or $created.ProcessId -le 0) {
    throw "WMI could not create the detached desktop-endurance supervisor (return $($created.ReturnValue))."
}

$supervisorStarted = $false
$childPid = $null
for ($attempt = 0; $attempt -lt 120; $attempt++) {
    Start-Sleep -Milliseconds 250
    try {
        $observed = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if (-not [string]::IsNullOrWhiteSpace([string]$observed.supervisor_error)) {
            throw "The WMI desktop-endurance supervisor failed before child launch: $($observed.supervisor_error)"
        }
        if ([int64]$observed.supervisor_pid -eq [int64]$created.ProcessId `
            -and $null -ne $observed.pid `
            -and [int64]$observed.pid -gt 0) {
            $supervisorStarted = $true
            $childPid = [int64]$observed.pid
            break
        }
    }
    catch {
    }
}
if (-not $supervisorStarted) {
    throw "The WMI desktop-endurance supervisor did not bind its process identity within 30 seconds."
}

Write-Output "desktop_endurance_capture=started launch_mode=wmi_detached_interactive_session logoff_resilient=false supervisor_pid=$($created.ProcessId) child_pid=$childPid manifest=$manifestPath stdout=$stdoutPath stderr=$stderrPath"
