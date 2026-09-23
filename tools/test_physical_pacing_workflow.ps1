$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$finalizer = Join-Path $PSScriptRoot "finalize_physical_pacing_matrix.ps1"
$verifier = Join-Path $PSScriptRoot "verify_physical_pacing_matrix.ps1"
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("asceify-physical-pacing-workflow-" + [Guid]::NewGuid().ToString("N"))
$null = New-Item -ItemType Directory -Path $testRoot

function Write-Json {
    param([string]$Path, [object]$Value)
    $json = $Value | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText($Path, $json + [Environment]::NewLine)
}

function Invoke-Finalizer {
    param([bool]$ExpectSuccess)
    $previousErrorPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $finalizer -SessionPath (Join-Path $testRoot "capture-session.json") *> $null
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorPreference
    if ($ExpectSuccess -and $exitCode -ne 0) {
        throw "Expected finalizer success."
    }
    if (-not $ExpectSuccess -and $exitCode -eq 0) {
        throw "Expected finalizer failure."
    }
}

function Invoke-Verifier {
    param([bool]$ExpectSuccess)
    $previousErrorPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $verifier -ManifestPath (Join-Path $testRoot "matrix.json") *> $null
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorPreference
    if ($ExpectSuccess -and $exitCode -ne 0) {
        throw "Expected verifier success."
    }
    if (-not $ExpectSuccess -and $exitCode -eq 0) {
        throw "Expected verifier failure."
    }
}

try {
    $binaryPath = Join-Path $testRoot "asceify_desktop.exe"
    $cargoLockPath = Join-Path $testRoot "Cargo.lock"
    [IO.File]::WriteAllBytes($binaryPath, [byte[]](9, 8, 7, 6))
    [IO.File]::WriteAllText($cargoLockPath, "fixture-lock")
    $binaryHash = (Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash
    $cargoLockHash = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
    $sourceRevision = "0123456789abcdef0123456789abcdef01234567"
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
        prepared_utc = "2030-01-01T00:00:00Z"
    }
    Write-Json (Join-Path $testRoot "capture-session.json") $session

    foreach ($target in @(60, 120, 144)) {
        $diagnostics = [ordered]@{
            schema_version = 3
            evidence_scope = "windowed_replay_to_frame_callback_and_native_compositor_timeline"
            source_revision = $sourceRevision
            physical_presentation_measured = $false
            external_scanout_instrumented = $false
            display = [ordered]@{
                outputs = @([ordered]@{ refresh_millihertz = $target * 1000 })
            }
        }
        Write-Json (Join-Path $testRoot ("diagnostics-{0}.json" -f $target)) $diagnostics
        $interval = [uint64](1000000000 / $target)
        $external = [ordered]@{
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
            instrument_name = "test-instrument"
            capture_id = "capture-$target"
            display_name = "test-display"
            display_device_path = "test-display-path"
            warmup_frames = 32
            sample_count = 256
            frame_interval_nanos = [ordered]@{ p50 = $interval; p95 = $interval; p99 = $interval; p99_9 = $interval; maximum = $interval }
            loss_recovery_counters = [ordered]@{ late_frames = 0; dropped_frames = 0; missed_frames = 0; recovery_events = 0 }
        }
        Write-Json (Join-Path $testRoot ("external-{0}.json" -f $target)) $external
    }

    Invoke-Finalizer $true
    if (-not (Test-Path -LiteralPath (Join-Path $testRoot "matrix.json"))) {
        throw "Successful finalization did not create matrix.json."
    }
    Invoke-Verifier $true
    [IO.File]::AppendAllText((Join-Path $testRoot "diagnostics-60.json"), " ")
    Invoke-Verifier $false
    $diagnostics60 = [ordered]@{
        schema_version = 3
        evidence_scope = "windowed_replay_to_frame_callback_and_native_compositor_timeline"
        source_revision = $sourceRevision
        physical_presentation_measured = $false
        external_scanout_instrumented = $false
        display = [ordered]@{ outputs = @([ordered]@{ refresh_millihertz = 60000 }) }
    }
    Write-Json (Join-Path $testRoot "diagnostics-60.json") $diagnostics60
    Remove-Item -LiteralPath (Join-Path $testRoot "matrix.json") -Force

    [IO.File]::AppendAllText((Join-Path $testRoot "external-60.json"), " ")
    Invoke-Finalizer $true
    Remove-Item -LiteralPath (Join-Path $testRoot "matrix.json") -Force

    $badExternal = Get-Content -LiteralPath (Join-Path $testRoot "external-120.json") -Raw | ConvertFrom-Json
    $badExternal.external_scanout_instrumented = $false
    Write-Json (Join-Path $testRoot "external-120.json") $badExternal
    Invoke-Finalizer $false
    if (Test-Path -LiteralPath (Join-Path $testRoot "matrix.json")) {
        throw "Failed finalization left a matrix manifest behind."
    }

    Write-Output "physical_pacing_workflow_self_tests=passed valid_matrix_finalized=true profile_hashes_pinned=true supporting_diagnostics_tamper_rejected=true invalid_external_capture_rejected=true failed_manifest_removed=true"
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
