$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$enduranceVerifier = Join-Path $PSScriptRoot "verify_desktop_endurance.ps1"
$pacingVerifier = Join-Path $PSScriptRoot "verify_physical_pacing_matrix.ps1"
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("axiusflow-evidence-verifier-" + [Guid]::NewGuid().ToString("N"))
$null = New-Item -ItemType Directory -Path $testRoot

function Write-Json {
    param([string]$Path, [object]$Value)
    $json = $Value | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText($Path, $json + [Environment]::NewLine)
}

function Invoke-ExpectedSuccess {
    param([string]$Script, [string]$Manifest)
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script -ManifestPath $Manifest
    if ($LASTEXITCODE -ne 0) {
        throw "Expected verifier success for $Manifest."
    }
}

function Invoke-ExpectedFailure {
    param([string]$Script, [string]$Manifest)
    $previousErrorPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script -ManifestPath $Manifest *> $null
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorPreference
    if ($exitCode -eq 0) {
        throw "Expected verifier failure for $Manifest."
    }
}

try {
    $binaryPath = Join-Path $testRoot "measured-binary.bin"
    [IO.File]::WriteAllBytes($binaryPath, [byte[]](1, 3, 3, 7))
    $binaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    $sourceRevision = "0123456789abcdef0123456789abcdef01234567"

    $reportPath = Join-Path $testRoot "endurance.json"
    $startedUtc = [DateTimeOffset]::Parse("2030-01-01T00:00:00Z")
    $report = [ordered]@{
        schema_version = 2
        evidence_scope = "headless_desktop_continuous_endurance"
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
        readiness_qualified = $true
    }
    Write-Json $reportPath $report
    $enduranceManifestPath = Join-Path $testRoot "endurance-manifest.json"
    $enduranceManifest = [ordered]@{
        schema_version = 1
        commit = $sourceRevision
        binary_sha256 = $binaryHash
        binary_path = "measured-binary.bin"
        report_path = "endurance.json"
        report_sha256 = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
        pid = 2147483647
        requested_duration_seconds = 28800
        started_utc = $startedUtc.ToString("O")
    }
    Write-Json $enduranceManifestPath $enduranceManifest
    Invoke-ExpectedSuccess $enduranceVerifier $enduranceManifestPath
    [IO.File]::AppendAllText($reportPath, " ")
    Invoke-ExpectedFailure $enduranceVerifier $enduranceManifestPath
    Write-Json $reportPath $report
    $enduranceManifest.report_sha256 = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
    Write-Json $enduranceManifestPath $enduranceManifest
    $enduranceManifest.pid = $PID
    Write-Json $enduranceManifestPath $enduranceManifest
    Invoke-ExpectedFailure $enduranceVerifier $enduranceManifestPath
    $enduranceManifest.pid = 2147483647
    $enduranceManifest.binary_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
    Write-Json $enduranceManifestPath $enduranceManifest
    Invoke-ExpectedFailure $enduranceVerifier $enduranceManifestPath
    $enduranceManifest.binary_sha256 = $binaryHash
    Write-Json $enduranceManifestPath $enduranceManifest
    $report.completion_state = "incomplete"
    $report.readiness_qualified = $false
    Write-Json $reportPath $report
    $enduranceManifest.report_sha256 = (Get-FileHash -LiteralPath $reportPath -Algorithm SHA256).Hash
    Write-Json $enduranceManifestPath $enduranceManifest
    Invoke-ExpectedFailure $enduranceVerifier $enduranceManifestPath
    $report.completion_state = "completed"
    $report.readiness_qualified = $true
    Write-Json $reportPath $report

    $profiles = @()
    foreach ($target in @(60, 120, 144)) {
        $evidencePath = Join-Path $testRoot ("physical-{0}.json" -f $target)
        $evidence = [ordered]@{
            schema_version = 1
            evidence_scope = "external_physical_scanout_pacing"
            source_revision = $sourceRevision
            binary_sha256 = $binaryHash
            target_refresh_hz = $target
            configured_refresh_millihertz = $target * 1000
            measured_refresh_millihertz = $target * 1000
            physical_presentation_measured = $true
            external_scanout_instrumented = $true
            measurement_method = "photodiode_scanout_capture"
            instrument_name = "deterministic-test-instrument"
            capture_id = "fixture-$target"
            display_name = "deterministic-display"
            display_device_path = "fixture-display-1"
            warmup_frames = 32
            sample_count = 256
            frame_interval_nanos = [ordered]@{
                p50 = [uint64](1000000000 / $target)
                p95 = [uint64](1000000000 / $target)
                p99 = [uint64](1000000000 / $target)
                p99_9 = [uint64](1000000000 / $target)
                maximum = [uint64](1000000000 / $target)
            }
            loss_recovery_counters = [ordered]@{
                late_frames = 0
                dropped_frames = 0
                missed_frames = 0
                recovery_events = 0
            }
        }
        Write-Json $evidencePath $evidence
        $profiles += [ordered]@{
            target_refresh_hz = $target
            evidence_path = [IO.Path]::GetFileName($evidencePath)
            evidence_sha256 = (Get-FileHash -LiteralPath $evidencePath -Algorithm SHA256).Hash
        }
    }
    $pacingManifestPath = Join-Path $testRoot "pacing-manifest.json"
    $pacingManifest = [ordered]@{
        schema_version = 1
        evidence_scope = "external_physical_scanout_pacing_matrix"
        source_revision = $sourceRevision
        binary_path = "measured-binary.bin"
        binary_sha256 = $binaryHash
        profiles = $profiles
    }
    Write-Json $pacingManifestPath $pacingManifest
    Invoke-ExpectedSuccess $pacingVerifier $pacingManifestPath

    $dwmOnly = [ordered]@{
        schema_version = 3
        evidence_scope = "windowed_replay_to_frame_callback_and_native_compositor_timeline"
        physical_presentation_measured = $false
        external_scanout_instrumented = $false
        presentation_measurement_method = "gpui_frame_callback_cadence_not_physical_scanout"
    }
    $dwmPath = Join-Path $testRoot "physical-60.json"
    Write-Json $dwmPath $dwmOnly
    $pacingManifest.profiles[0].evidence_sha256 = (Get-FileHash -LiteralPath $dwmPath -Algorithm SHA256).Hash
    Write-Json $pacingManifestPath $pacingManifest
    Invoke-ExpectedFailure $pacingVerifier $pacingManifestPath

    Write-Output "evidence_verifier_self_tests=passed endurance_valid=true endurance_active_rejected=true endurance_binary_hash_mismatch_rejected=true endurance_report_tamper_rejected=true endurance_incomplete_rejected=true physical_matrix_valid=true dwm_only_rejected=true"
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
