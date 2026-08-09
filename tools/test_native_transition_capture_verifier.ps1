$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$verifier = Join-Path $PSScriptRoot "verify_native_transition_capture.ps1"
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("axiusflow-native-transition-verifier-" + [Guid]::NewGuid().ToString("N"))
$null = New-Item -ItemType Directory -Path $testRoot
$artifact = Join-Path $testRoot "native-transitions.json"
$manifestPath = "$artifact.manifest.json"
$cargoLockPath = Join-Path $testRoot "Cargo.lock"
$executablePath = Join-Path $testRoot "axiusflow_desktop.exe"

function Write-Json {
    param([string]$Path, [object]$Value)
    [IO.File]::WriteAllText(
        $Path,
        ($Value | ConvertTo-Json -Depth 10) + [Environment]::NewLine
    )
}

function Invoke-ExpectedSuccess {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $verifier -ArtifactPath $artifact -ManifestPath $manifestPath
    if ($LASTEXITCODE -ne 0) {
        throw "Expected native-transition verifier success."
    }
}

function Invoke-ExpectedFailure {
    $previousErrorPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $verifier -ArtifactPath $artifact -ManifestPath $manifestPath *> $null
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousErrorPreference
    if ($exitCode -eq 0) {
        throw "Expected native-transition verifier failure."
    }
}

function New-ReadyState {
    return [ordered]@{
        selection_installed = $true
        instrument_installed = $true
        history_request_active = $false
        live_chart_installed = $true
        pending_live_request = $false
        buffered_history_trades = 0
        history_trade_overflow = $false
        depth_selection_installed = $true
    }
}

function New-ClearedState {
    return [ordered]@{
        selection_installed = $false
        instrument_installed = $false
        history_request_active = $false
        live_chart_installed = $false
        pending_live_request = $false
        buffered_history_trades = 0
        history_trade_overflow = $false
        depth_selection_installed = $false
    }
}

function New-Transition {
    param(
        [uint64]$LossOrdinal,
        [uint64]$RestorationOrdinal,
        [uint64]$RetiredGeneration,
        [uint64]$FreshGeneration,
        [uint64]$BaseTime
    )
    return [ordered]@{
        loss_callback_observed = $true
        loss_source_ordinal = $LossOrdinal
        loss_unix_milliseconds = $BaseTime
        retired_generation = $RetiredGeneration
        environment_apply_succeeded = $true
        session_stop_confirmed = $true
        provider_invalidation_preceded_fence = $false
        pre_transition_state = New-ReadyState
        retired_state_cleared = $true
        post_retirement_state = New-ClearedState
        restoration_callback_observed = $true
        restoration_source_ordinal = $RestorationOrdinal
        restoration_unix_milliseconds = $BaseTime + 100
        fresh_generation = $FreshGeneration
        authentication_accepted = $true
        authentication_unix_milliseconds = $BaseTime + 200
        runtime_rehydrated = $true
        rehydrated_unix_milliseconds = $BaseTime + 300
        restored_runtime_state = New-ReadyState
        completed = $true
    }
}

function New-StartupRecovery {
    return [ordered]@{
        restoration_callback_observed = $true
        restoration_source_ordinal = 1
        restoration_unix_milliseconds = 1500
        fresh_generation = 7
        authentication_accepted = $true
        authentication_unix_milliseconds = 1600
        runtime_rehydrated = $true
        rehydrated_unix_milliseconds = 1700
        restored_runtime_state = New-ReadyState
        completed = $true
    }
}

try {
    [IO.File]::WriteAllText($cargoLockPath, "deterministic-lock-fixture")
    [IO.File]::WriteAllBytes($executablePath, [byte[]](1, 3, 3, 7))
    $sourceRevision = "0123456789abcdef0123456789abcdef01234567"
    $cargoLockHash = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
    $executableHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
    $report = [ordered]@{
        schema_version = 2
        evidence_scope = "rithmic_test_physical_native_transition_capture"
        platform = "windows"
        completion_state = "completed"
        checkpoint_sequence = 12
        created_unix_milliseconds = 1000
        checkpoint_unix_milliseconds = 5000
        source_revision = $sourceRevision
        clean_worktree = $true
        cargo_lock_path = $cargoLockPath
        cargo_lock_sha256 = $cargoLockHash
        executable_path = $executablePath
        executable_sha256 = $executableHash
        callback_source = "NativeNetworkMonitor_and_NativePowerMonitor"
        shipping_mode = "rithmic_test_existing_native_vault_worker"
        credential_source = "native_vault"
        credentials_embedded = $false
        transitions_triggered_by_capture = $false
        initial_network_state = "unavailable"
        offline_startup_observed = $true
        offline_startup_recovery = New-StartupRecovery
        observer_overflow = $false
        observer_overflow_count = 0
        monitor_failures = 0
        callbacks_received = 5
        callbacks_applied = 5
        callback_application_failures = 0
        network_offline = New-Transition 2 3 7 8 2000
        suspend_resume = New-Transition 4 5 8 9 3000
        scenario_requirements_met = $true
        worker_clean_stop = $true
        finalized = $true
        readiness_qualified = $true
    }
    Write-Json $artifact $report
    $manifest = [ordered]@{
        schema_version = 1
        evidence_scope = "rithmic_test_native_transition_capture_manifest"
        source_revision = $sourceRevision
        clean_worktree = $true
        cargo_lock_path = $cargoLockPath
        cargo_lock_sha256 = $cargoLockHash
        executable_path = $executablePath
        executable_sha256 = $executableHash
        report_path = $artifact
        started_utc = "2030-01-01T00:00:00Z"
        finalized = $true
        final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
        process_exit_code = 0
        finalized_utc = "2030-01-01T00:10:00Z"
    }
    Write-Json $manifestPath $manifest
    Invoke-ExpectedSuccess

    [IO.File]::AppendAllText($artifact, " ")
    Invoke-ExpectedFailure
    Write-Json $artifact $report

    $report.completion_state = "incomplete"
    $report.readiness_qualified = $false
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.completion_state = "completed"
    $report.readiness_qualified = $true

    $report.initial_network_state = "available"
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.initial_network_state = "unavailable"

    $report.offline_startup_observed = $false
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.offline_startup_observed = $true

    $report.suspend_resume.provider_invalidation_preceded_fence = $true
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.suspend_resume.provider_invalidation_preceded_fence = $false

    $report.suspend_resume.retired_generation = 99
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.suspend_resume.retired_generation = 8

    $report.suspend_resume.loss_source_ordinal = 3
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.suspend_resume.loss_source_ordinal = 4

    $report.monitor_failures = 1
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure
    $report.monitor_failures = 0

    $report.observer_overflow = $true
    $report.observer_overflow_count = 1
    Write-Json $artifact $report
    $manifest.final_report_sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    Write-Json $manifestPath $manifest
    Invoke-ExpectedFailure

    Write-Output "native_transition_verifier_self_tests=passed valid=true artifact_tamper_rejected=true incomplete_rejected=true online_startup_rejected=true offline_startup_flag_required=true provider_invalidation_precedence_rejected=true generation_discontinuity_rejected=true overlapping_sequence_rejected=true monitor_failure_rejected=true observer_overflow_rejected=true"
}
finally {
    if (Test-Path -LiteralPath $testRoot) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}
