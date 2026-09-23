$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($args.Count -ne 0) {
    throw "This command accepts no arguments; Rithmic Test credentials are loaded only from the native vault."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
cargo run `
    --manifest-path (Join-Path $repoRoot "Cargo.toml") `
    --locked `
    --package asceify_rithmic_protocol_adapter `
    --bin rithmic_test_smoke `
    -- `
    --authorized-silence-recovery
if ($LASTEXITCODE -ne 0) {
    throw "Authorized Rithmic silence/recovery evidence failed."
}

Write-Output "rithmic_authorized_silence_recovery=passed credentials=native_vault authorized_client_local_silence=true provider_observed_loss=false covering_recovery=deterministic_production_state_machine clean_close=true"
