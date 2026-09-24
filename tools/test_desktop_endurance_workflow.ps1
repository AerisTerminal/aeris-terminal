[CmdletBinding()]
param(
    [switch]$LaunchWmiSmoke
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$scripts = @(
    "run_desktop_endurance.ps1",
    "supervise_desktop_endurance.ps1",
    "finalize_desktop_endurance.ps1",
    "verify_desktop_endurance.ps1"
)
foreach ($script in $scripts) {
    $tokens = $null
    $errors = $null
    $path = Join-Path $PSScriptRoot $script
    [Management.Automation.Language.Parser]::ParseFile(
        $path,
        [ref]$tokens,
        [ref]$errors
    ) > $null
    if ($errors.Count -ne 0) {
        throw "PowerShell parse failed for $script."
    }
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class AerisEndurancePowerSmoke
{
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint SetThreadExecutionState(uint executionState);
}
'@
$sleepInhibited = $false
$sleepInhibitionReleased = $false
try {
    $sleepInhibited = [AerisEndurancePowerSmoke]::SetThreadExecutionState([uint32]2147483649) -ne 0
    if (-not $sleepInhibited) {
        throw "SetThreadExecutionState could not inhibit system sleep."
    }
}
finally {
    $sleepInhibitionReleased = [AerisEndurancePowerSmoke]::SetThreadExecutionState([uint32]2147483648) -ne 0
}
if (-not $sleepInhibitionReleased) {
    throw "SetThreadExecutionState could not restore the continuous state."
}

function Write-Json {
    param([string]$Path, [object]$Value)
    [IO.File]::WriteAllText(
        $Path,
        ($Value | ConvertTo-Json -Depth 10) + [Environment]::NewLine
    )
}

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("aeris-endurance-workflow-" + [Guid]::NewGuid().ToString("N"))
$null = New-Item -ItemType Directory -Path $testRoot
try {
    $capturedSupervisor = Join-Path $testRoot "supervise_desktop_endurance.ps1"
    $capturedFinalizer = Join-Path $testRoot "finalize_desktop_endurance.ps1"
    $capturedVerifier = Join-Path $testRoot "verify_desktop_endurance.ps1"
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "supervise_desktop_endurance.ps1") -Destination $capturedSupervisor
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "finalize_desktop_endurance.ps1") -Destination $capturedFinalizer
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "verify_desktop_endurance.ps1") -Destination $capturedVerifier

    $binaryPath = Join-Path $testRoot "aeris_desktop.exe"
    $cargoLockPath = Join-Path $testRoot "Cargo.lock"
    $stdoutPath = Join-Path $testRoot "stdout.log"
    $stderrPath = Join-Path $testRoot "stderr.log"
    $reportPath = Join-Path $testRoot "desktop-endurance-schema2-8h.json"
    $manifestPath = Join-Path $testRoot "run-manifest.json"
    [IO.File]::WriteAllBytes($binaryPath, [byte[]](2, 4, 8, 16))
    [IO.File]::WriteAllText($cargoLockPath, "fixture-lock")
    [IO.File]::WriteAllText($stdoutPath, "completed")
    [IO.File]::WriteAllText($stderrPath, "")

    $startedUtc = [DateTimeOffset]::Parse("2020-01-01T00:00:00Z")
    $report = [ordered]@{
        schema_version = 2
        evidence_scope = "synthetic_headless_desktop_continuous_endurance"
        completion_state = "completed"
        checkpoint_sequence = 481
        checkpoint_unix_milliseconds = $startedUtc.ToUnixTimeMilliseconds() + 28800001
        requested_duration_seconds = 28800
        required_qualification_duration_seconds = 28800
        elapsed_milliseconds = 28800001
        frame_cycles = 1800000
        updates_published = 31800000
        mailbox_capacity = 32
        mailbox_high_water_items = 1
        last_generation = 31800000
        stale_or_gapped_publications = 0
        working_set_baseline_bytes = 12000000
        working_set_current_bytes = 13000000
        working_set_sampled_high_water_bytes = 14000000
        maximum_working_set_growth_bytes = 67108864
        working_set_within_bound = $true
        clean_stop = $true
        synthetic_bounds_qualified = $true
        live_market_gate = "not_run"
    }
    Write-Json $reportPath $report
    $manifest = [ordered]@{
        schema_version = 2
        evidence_scope = "desktop_endurance_capture_manifest"
        commit = "0123456789abcdef0123456789abcdef01234567"
        clean_worktree = $true
        cargo_lock_path = $cargoLockPath
        cargo_lock_sha256 = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
        binary_path = $binaryPath
        binary_sha256 = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
        supervisor_script_path = $capturedSupervisor
        supervisor_script_sha256 = (Get-FileHash -LiteralPath $capturedSupervisor -Algorithm SHA256).Hash
        finalizer_script_path = $capturedFinalizer
        finalizer_script_sha256 = (Get-FileHash -LiteralPath $capturedFinalizer -Algorithm SHA256).Hash
        verifier_script_path = $capturedVerifier
        verifier_script_sha256 = (Get-FileHash -LiteralPath $capturedVerifier -Algorithm SHA256).Hash
        report_path = $reportPath
        report_sha256 = $null
        stdout_path = $stdoutPath
        stdout_sha256 = $null
        stderr_path = $stderrPath
        stderr_sha256 = $null
        pid = 2147483647
        supervisor_pid = 2147483646
        requested_duration_seconds = 28800
        started_utc = $startedUtc.ToString("O")
        process_exit_code = $null
        process_exit_evidence = $null
        finalization_mode = $null
        launch_mode = "foreground_supervisor"
        logoff_resilient = $false
        system_sleep_inhibited = $true
        system_sleep_inhibition_released = $false
        sleep_inhibition_release_evidence = $null
        finalized = $false
        finalized_utc = $null
    }
    Write-Json $manifestPath $manifest
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $capturedFinalizer -ManifestPath $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Captured desktop-endurance recovery finalizer failed."
    }
    $finalizedManifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if (-not [bool]$finalizedManifest.finalized `
        -or $finalizedManifest.process_exit_evidence -ne "completed_report_recovery" `
        -or $finalizedManifest.finalization_mode -ne "recovered_after_supervisor_loss" `
        -or $null -ne $finalizedManifest.process_exit_code) {
        throw "Desktop-endurance recovery finalization did not preserve honest exit evidence."
    }
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}

$wmiLaunched = $false
if ($LaunchWmiSmoke) {
    $currentSessionId = (Get-Process -Id $PID).SessionId
    $created = Invoke-CimMethod `
        -ClassName Win32_Process `
        -MethodName Create `
        -Arguments @{ CommandLine = 'powershell.exe -NoProfile -NonInteractive -Command "Start-Sleep -Seconds 2"' }
    if ($created.ReturnValue -ne 0 -or $created.ProcessId -le 0) {
        throw "WMI detached-process smoke could not create its process."
    }
    $wmiLaunched = $true
    $createdProcess = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId = $($created.ProcessId)"
    if ($null -eq $createdProcess `
        -or [int]$createdProcess.SessionId -ne $currentSessionId `
        -or $createdProcess.ParentProcessId -eq $PID) {
        throw "WMI detached-process smoke did not establish the expected detached interactive-session boundary."
    }
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        Start-Sleep -Milliseconds 250
        if ($null -eq (Get-Process -Id $created.ProcessId -ErrorAction SilentlyContinue)) {
            break
        }
    }
    if ($null -ne (Get-Process -Id $created.ProcessId -ErrorAction SilentlyContinue)) {
        throw "WMI detached-process smoke did not stop naturally."
    }
}

Write-Output "desktop_endurance_workflow_self_test=passed scripts_parse=true frozen_recovery_finalization=true recovery_exit_code_invented=false sleep_inhibition=true sleep_inhibition_released=true detached_boundary=wmi_interactive_session logoff_resilient=false wmi_launched=$($wmiLaunched.ToString().ToLowerInvariant())"
