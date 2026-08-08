<#
.SYNOPSIS
Verifies completed physical Windows offline-startup, network-offline, and suspend/resume evidence.

.DESCRIPTION
The capture is produced only by the committed launcher and opt-in Rithmic Test
shipping worker. This verifier checks its finalized provenance manifest, typed native callback ordinals,
confirmed generation retirement, complete state clearing, strictly newer native-
vault reconnection, authentication, and restored chart/DOM/selection state. A real
initial NativeNetworkMonitor Unavailable result is required in addition to the
later online-ready loss/recovery sequence. The verifier never changes network or
power state.

.EXAMPLE
powershell -File tools/verify_native_transition_capture.ps1 -ArtifactPath local-data/evidence/native-transitions.json -ManifestPath local-data/evidence/native-transitions.json.manifest.json
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactPath,

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

function Assert-JsonTrue {
    param([object]$Value, [string]$Name)
    Assert-True ($Value -is [bool] -and $Value) "$Name must be the JSON boolean true."
}

function Assert-JsonFalse {
    param([object]$Value, [string]$Name)
    Assert-True ($Value -is [bool] -and -not $Value) "$Name must be the JSON boolean false."
}

function Assert-NonnegativeInteger {
    param([object]$Value, [string]$Name)
    $converted = 0L
    Assert-True ($Value -isnot [string] -and $Value -isnot [bool] -and [Int64]::TryParse([string]$Value, [ref]$converted) -and $converted -ge 0) "$Name must be a nonnegative JSON integer."
}

function Assert-ReadyState {
    param([object]$State, [string]$Name)
    Assert-JsonTrue (Get-RequiredProperty $State "selection_installed") "$Name.selection_installed"
    Assert-JsonTrue (Get-RequiredProperty $State "instrument_installed") "$Name.instrument_installed"
    Assert-JsonFalse (Get-RequiredProperty $State "history_request_active") "$Name.history_request_active"
    Assert-JsonTrue (Get-RequiredProperty $State "live_chart_installed") "$Name.live_chart_installed"
    Assert-JsonFalse (Get-RequiredProperty $State "pending_live_request") "$Name.pending_live_request"
    Assert-True ((Get-RequiredProperty $State "buffered_history_trades") -eq 0) "$Name buffered trades must be empty."
    Assert-JsonFalse (Get-RequiredProperty $State "history_trade_overflow") "$Name.history_trade_overflow"
    Assert-JsonTrue (Get-RequiredProperty $State "depth_selection_installed") "$Name.depth_selection_installed"
}

function Assert-ClearedState {
    param([object]$State, [string]$Name)
    foreach ($property in @("selection_installed", "instrument_installed", "history_request_active", "live_chart_installed", "pending_live_request", "history_trade_overflow", "depth_selection_installed")) {
        Assert-JsonFalse (Get-RequiredProperty $State $property) "$Name.$property"
    }
    Assert-True ((Get-RequiredProperty $State "buffered_history_trades") -eq 0) "$Name buffered trades must be empty."
}

function Assert-Transition {
    param([object]$Transition, [string]$Name, [uint64]$CheckpointMilliseconds)
    foreach ($property in @("loss_callback_observed", "environment_apply_succeeded", "session_stop_confirmed", "retired_state_cleared", "restoration_callback_observed", "authentication_accepted", "runtime_rehydrated", "completed")) {
        Assert-JsonTrue (Get-RequiredProperty $Transition $property) "$Name.$property"
    }
    Assert-JsonFalse (Get-RequiredProperty $Transition "provider_invalidation_preceded_fence") "$Name.provider_invalidation_preceded_fence"
    $lossOrdinal = Get-RequiredProperty $Transition "loss_source_ordinal"
    $restorationOrdinal = Get-RequiredProperty $Transition "restoration_source_ordinal"
    $retiredGeneration = Get-RequiredProperty $Transition "retired_generation"
    $freshGeneration = Get-RequiredProperty $Transition "fresh_generation"
    foreach ($entry in @(
        @($lossOrdinal, "$Name.loss_source_ordinal"),
        @($restorationOrdinal, "$Name.restoration_source_ordinal"),
        @($retiredGeneration, "$Name.retired_generation"),
        @($freshGeneration, "$Name.fresh_generation")
    )) {
        Assert-NonnegativeInteger $entry[0] $entry[1]
        Assert-True ([uint64]$entry[0] -gt 0) "$($entry[1]) must be positive."
    }
    Assert-True ([uint64]$restorationOrdinal -gt [uint64]$lossOrdinal) "$Name restoration callback must follow its loss callback."
    Assert-True ([uint64]$freshGeneration -gt [uint64]$retiredGeneration) "$Name restoration must use a strictly newer generation."

    $lossTime = [uint64](Get-RequiredProperty $Transition "loss_unix_milliseconds")
    $restorationTime = [uint64](Get-RequiredProperty $Transition "restoration_unix_milliseconds")
    $authenticationTime = [uint64](Get-RequiredProperty $Transition "authentication_unix_milliseconds")
    $rehydratedTime = [uint64](Get-RequiredProperty $Transition "rehydrated_unix_milliseconds")
    Assert-True ($lossTime -le $restorationTime -and $restorationTime -le $authenticationTime -and $authenticationTime -le $rehydratedTime -and $rehydratedTime -le $CheckpointMilliseconds) "$Name timestamps are not ordered through the final checkpoint."
    Assert-ReadyState (Get-RequiredProperty $Transition "pre_transition_state") "$Name.pre_transition_state"
    Assert-ClearedState (Get-RequiredProperty $Transition "post_retirement_state") "$Name.post_retirement_state"
    Assert-ReadyState (Get-RequiredProperty $Transition "restored_runtime_state") "$Name.restored_runtime_state"
}

$resolvedPath = [IO.Path]::GetFullPath($ArtifactPath)
Assert-True (Test-Path -LiteralPath $resolvedPath -PathType Leaf) "Native-transition artifact does not exist: $resolvedPath"
$resolvedManifestPath = [IO.Path]::GetFullPath($ManifestPath)
Assert-True (Test-Path -LiteralPath $resolvedManifestPath -PathType Leaf) "Native-transition manifest does not exist: $resolvedManifestPath"
try {
    $manifest = Get-Content -LiteralPath $resolvedManifestPath -Raw | ConvertFrom-Json
}
catch {
    throw "Native-transition manifest is not valid JSON: $resolvedManifestPath"
}
Assert-True ((Get-RequiredProperty $manifest "schema_version") -eq 1) "Native-transition manifest schema_version must be 1."
Assert-True ((Get-RequiredProperty $manifest "evidence_scope") -eq "rithmic_test_native_transition_capture_manifest") "Native-transition manifest evidence_scope is invalid."
$sourceRevision = [string](Get-RequiredProperty $manifest "source_revision")
Assert-True ($sourceRevision -match '^[0-9a-fA-F]{40}$') "Manifest source_revision must be a full Git revision."
Assert-JsonTrue (Get-RequiredProperty $manifest "clean_worktree") "manifest.clean_worktree"
Assert-JsonTrue (Get-RequiredProperty $manifest "finalized") "manifest.finalized"
Assert-True ((Get-RequiredProperty $manifest "process_exit_code") -eq 0) "Capture process did not exit successfully."
$manifestReportPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "report_path"))
Assert-True ($manifestReportPath -eq $resolvedPath) "Manifest report_path does not match ArtifactPath."
$expectedReportHash = [string](Get-RequiredProperty $manifest "final_report_sha256")
Assert-True ($expectedReportHash -match '^[0-9a-fA-F]{64}$') "Manifest final_report_sha256 is invalid."
$actualHash = (Get-FileHash -LiteralPath $resolvedPath -Algorithm SHA256).Hash
Assert-True ($actualHash -ieq $expectedReportHash) "Native-transition artifact SHA-256 does not match its finalized manifest."
$cargoLockPath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "cargo_lock_path"))
$executablePath = [IO.Path]::GetFullPath([string](Get-RequiredProperty $manifest "executable_path"))
Assert-True (Test-Path -LiteralPath $cargoLockPath -PathType Leaf) "Manifest Cargo.lock does not exist."
Assert-True (Test-Path -LiteralPath $executablePath -PathType Leaf) "Manifest executable does not exist."
$cargoLockHash = (Get-FileHash -LiteralPath $cargoLockPath -Algorithm SHA256).Hash
$executableHash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash
Assert-True ($cargoLockHash -ieq [string](Get-RequiredProperty $manifest "cargo_lock_sha256")) "Cargo.lock SHA-256 does not match the manifest."
Assert-True ($executableHash -ieq [string](Get-RequiredProperty $manifest "executable_sha256")) "Executable SHA-256 does not match the manifest."
try {
    $report = Get-Content -LiteralPath $resolvedPath -Raw | ConvertFrom-Json
}
catch {
    throw "Native-transition artifact is not valid JSON: $resolvedPath"
}

Assert-True ((Get-RequiredProperty $report "schema_version") -eq 1) "Native-transition schema_version must be 1."
Assert-True ((Get-RequiredProperty $report "evidence_scope") -eq "rithmic_test_physical_native_transition_capture") "Native-transition evidence_scope is invalid."
Assert-True ((Get-RequiredProperty $report "platform") -eq "windows") "Physical native-transition evidence must be captured on Windows."
Assert-True ((Get-RequiredProperty $report "completion_state") -eq "completed") "Native-transition capture is incomplete."
Assert-True ((Get-RequiredProperty $report "source_revision") -ieq $sourceRevision) "Report source revision does not match its manifest."
Assert-JsonTrue (Get-RequiredProperty $report "clean_worktree") "report.clean_worktree"
Assert-True ([IO.Path]::GetFullPath([string](Get-RequiredProperty $report "cargo_lock_path")) -eq $cargoLockPath) "Report Cargo.lock path does not match its manifest."
Assert-True ([IO.Path]::GetFullPath([string](Get-RequiredProperty $report "executable_path")) -eq $executablePath) "Report executable path does not match its manifest."
Assert-True ((Get-RequiredProperty $report "cargo_lock_sha256") -ieq $cargoLockHash) "Report Cargo.lock hash does not match."
Assert-True ((Get-RequiredProperty $report "executable_sha256") -ieq $executableHash) "Report executable hash does not match."
Assert-True ((Get-RequiredProperty $report "callback_source") -eq "NativeNetworkMonitor_and_NativePowerMonitor") "Native-transition callback source is invalid."
Assert-True ((Get-RequiredProperty $report "shipping_mode") -eq "rithmic_test_existing_native_vault_worker") "Capture did not use the existing Rithmic Test shipping worker."
Assert-True ((Get-RequiredProperty $report "credential_source") -eq "native_vault") "Capture credential source is invalid."
Assert-JsonFalse (Get-RequiredProperty $report "credentials_embedded") "credentials_embedded"
Assert-JsonFalse (Get-RequiredProperty $report "transitions_triggered_by_capture") "transitions_triggered_by_capture"
Assert-JsonFalse (Get-RequiredProperty $report "observer_overflow") "observer_overflow"
Assert-True ((Get-RequiredProperty $report "observer_overflow_count") -eq 0) "Native-transition observer overflow count must be zero."
Assert-True ((Get-RequiredProperty $report "callback_application_failures") -eq 0) "Native-transition callback application failures must be zero."
$received = Get-RequiredProperty $report "callbacks_received"
$applied = Get-RequiredProperty $report "callbacks_applied"
Assert-NonnegativeInteger $received "callbacks_received"
Assert-NonnegativeInteger $applied "callbacks_applied"
Assert-True ([uint64]$received -ge 4 -and $received -eq $applied) "Every received native callback must be applied and at least four are required."
$initialNetwork = [string](Get-RequiredProperty $report "initial_network_state")
Assert-True ($initialNetwork -eq "unavailable") "Native-transition evidence requires an initial NativeNetworkMonitor Unavailable result."
Assert-JsonTrue (Get-RequiredProperty $report "offline_startup_observed") "offline_startup_observed"
$created = [uint64](Get-RequiredProperty $report "created_unix_milliseconds")
$checkpoint = [uint64](Get-RequiredProperty $report "checkpoint_unix_milliseconds")
Assert-True ($checkpoint -ge $created) "Final checkpoint predates capture creation."
Assert-Transition (Get-RequiredProperty $report "network_offline") "network_offline" $checkpoint
Assert-Transition (Get-RequiredProperty $report "suspend_resume") "suspend_resume" $checkpoint
Assert-JsonTrue (Get-RequiredProperty $report "scenario_requirements_met") "scenario_requirements_met"
Assert-JsonTrue (Get-RequiredProperty $report "worker_clean_stop") "worker_clean_stop"
Assert-JsonTrue (Get-RequiredProperty $report "finalized") "report.finalized"
Assert-JsonTrue (Get-RequiredProperty $report "readiness_qualified") "readiness_qualified"

Write-Output "native_transition_capture=verified physical_offline_startup=true physical_network_offline=true physical_suspend_resume=true source_ordinals=true native_vault=true artifact_sha256=$($actualHash.ToUpperInvariant()) artifact=$resolvedPath"
