<#
.SYNOPSIS
Owns one prepared desktop-endurance process and records its exact exit state.

.DESCRIPTION
This internal worker is invoked by run_desktop_endurance.ps1 either directly or
through WMI detachment. It never builds or selects artifacts.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath
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

function Get-RequiredProperty {
    param([object]$Object, [string]$Name)
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        throw "Missing required manifest property '$Name'."
    }
    return $property.Value
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class TradingPlotEndurancePower
{
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint SetThreadExecutionState(uint executionState);
}
'@

$resolvedManifestPath = [IO.Path]::GetFullPath($ManifestPath)
$manifest = Get-Content -LiteralPath $resolvedManifestPath -Raw | ConvertFrom-Json

try {
    if ((Get-RequiredProperty $manifest "schema_version") -ne 2 `
        -or (Get-RequiredProperty $manifest "evidence_scope") -ne "desktop_endurance_capture_manifest") {
        throw "Desktop-endurance supervisor requires a schema-2 capture manifest."
    }
    if ([bool](Get-RequiredProperty $manifest "finalized") `
        -or $null -ne (Get-RequiredProperty $manifest "pid")) {
        throw "Desktop-endurance manifest was already launched or finalized."
    }

    foreach ($scriptName in @("supervisor", "finalizer", "verifier")) {
        $scriptPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "${scriptName}_script_path"))
        $expectedScriptHash = [string](Get-RequiredProperty $manifest "${scriptName}_script_sha256")
        if (-not (Test-Path -LiteralPath $scriptPath -PathType Leaf) `
            -or (Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash -ine $expectedScriptHash) {
            throw "Captured desktop-endurance $scriptName script provenance is invalid."
        }
        if ($scriptName -eq "supervisor" -and $scriptPath -ne [IO.Path]::GetFullPath($PSCommandPath)) {
            throw "Desktop-endurance supervisor is not running from the captured artifact."
        }
    }

    $manifest.supervisor_pid = $PID
    Write-JsonAtomically $resolvedManifestPath $manifest

    $launchMode = [string](Get-RequiredProperty $manifest "launch_mode")
    if ($launchMode -ne "foreground_supervisor" `
        -and $launchMode -ne "wmi_detached_interactive_session") {
        throw "Desktop-endurance launch mode is invalid."
    }

    $binaryPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "binary_path"))
    $reportPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "report_path"))
    $stdoutPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "stdout_path"))
    $stderrPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "stderr_path"))
    if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
        throw "Desktop-endurance executable is missing: $binaryPath"
    }
    $actualBinaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    if ($actualBinaryHash -ine [string](Get-RequiredProperty $manifest "binary_sha256")) {
        throw "Desktop-endurance executable hash changed before launch."
    }

    $executionState = [TradingPlotEndurancePower]::SetThreadExecutionState([uint32]2147483649)
    if ($executionState -eq 0) {
        throw "Desktop-endurance supervisor could not inhibit system sleep."
    }
    $manifest.system_sleep_inhibited = $true
    Write-JsonAtomically $resolvedManifestPath $manifest

    $process = $null
    try {
        $manifest.started_utc = [DateTimeOffset]::UtcNow.ToString("O")
        $quotedReportPath = '"' + $reportPath + '"'
        $process = Start-Process -FilePath $binaryPath `
            -ArgumentList @("--desktop-endurance", $quotedReportPath, "28800") `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath `
            -WindowStyle Hidden `
            -PassThru
        $manifest.pid = $process.Id
        Write-JsonAtomically $resolvedManifestPath $manifest

        $process.WaitForExit()
        $manifest.process_exit_code = $process.ExitCode
        $manifest.process_exit_evidence = if ($process.ExitCode -eq 0) {
            "supervisor_observed_zero"
        }
        else {
            "supervisor_observed_nonzero"
        }
        $manifest.finalization_mode = "supervised"
    }
    finally {
        $releaseState = [TradingPlotEndurancePower]::SetThreadExecutionState([uint32]2147483648)
        $manifest.system_sleep_inhibition_released = $releaseState -ne 0
        $manifest.sleep_inhibition_release_evidence = if ($releaseState -ne 0) {
            "explicit_es_continuous"
        }
        else {
            "explicit_release_failed"
        }
        Write-JsonAtomically $resolvedManifestPath $manifest
    }

    if (-not [bool]$manifest.system_sleep_inhibition_released) {
        throw "Desktop-endurance supervisor could not restore the thread execution state."
    }

    if ($process.ExitCode -ne 0) {
        throw "Desktop-endurance process exited with code $($process.ExitCode)."
    }

    $finalizer = Join-Path $PSScriptRoot "finalize_desktop_endurance.ps1"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $finalizer -ManifestPath $resolvedManifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Desktop-endurance finalization failed."
    }
}
catch {
    $manifest.supervisor_error = $_.Exception.Message
    Write-JsonAtomically $resolvedManifestPath $manifest
    throw
}
