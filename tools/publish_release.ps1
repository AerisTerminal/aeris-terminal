param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [UInt64]::MaxValue)]
    [UInt64]$Generation,

    [string]$SigningKeyFile = (Join-Path $env:LOCALAPPDATA "Axiusflow\release-signing-key.b64"),

    [ValidatePattern('^[A-Za-z0-9._-]{1,32}$')]
    [string]$Channel = "stable"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$websiteRepo = Join-Path (Split-Path -Parent $repo) "axiusflow-website"
$wrangler = Join-Path $websiteRepo "node_modules\.bin\wrangler.cmd"
$baseUrl = "https://auth.axiusflow.com/releases"
$bucket = "axiusflow-releases"

if (-not (Test-Path -LiteralPath $SigningKeyFile -PathType Leaf)) {
    throw "Release signing key is missing at '$SigningKeyFile'. Refusing to create or rotate production trust implicitly."
}

if (-not (Test-Path -LiteralPath $wrangler -PathType Leaf)) {
    throw "Wrangler is unavailable at '$wrangler'. Install website dependencies before publishing."
}

Push-Location $repo
try {
    $status = (& git status --porcelain --untracked-files=normal | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to inspect the Git worktree."
    }
    if ($status) {
        throw "Production release requires a clean Git worktree. Commit or remove local changes first."
    }

    $identity = (& git rev-parse HEAD | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $identity -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw "Unable to resolve the release identity from Git HEAD."
    }

    Write-Host "Qualifying Axiusflow release $Generation at $identity"
    & cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw "cargo fmt gate failed." }

    & cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "cargo clippy gate failed." }

    & cargo test --workspace --all-features --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo test gate failed." }

    $publishedAt = [DateTime]::UtcNow.ToString(
        "yyyy-MM-dd'T'HH:mm:ss'Z'",
        [Globalization.CultureInfo]::InvariantCulture
    )
    $output = Join-Path $repo ("target\release-publish\{0}-{1}" -f $Generation, $identity)

    Write-Host "Building, signing, uploading, and verifying immutable release objects"
    & cargo run --locked --release -p axiusflow_platform_runtime --bin axiusflow_release_publisher -- `
        --signing-key-file $SigningKeyFile `
        --release-identity $identity `
        --generation $Generation `
        --base-url $baseUrl `
        --published-at $publishedAt `
        --channel $Channel `
        --output $output `
        --r2-bucket $bucket `
        --wrangler $wrangler
    if ($LASTEXITCODE -ne 0) {
        throw "Release publication failed. The stable channel is unchanged unless the publisher completed successfully."
    }

    Write-Host "Release $Generation published. Verify the public channel and installer before removing any previous artifact."
}
finally {
    Pop-Location
}
