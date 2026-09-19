param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [UInt64]::MaxValue)]
    [UInt64]$Generation,

    [string]$ReleaseVersion,

    [string]$MinimumLauncherVersion = "0.2.0",

    [string]$SigningKeyFile = (Join-Path $env:LOCALAPPDATA "Axiusflow\release-signing-key.b64"),

    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$AuthenticodeCertificateSha1,

    [ValidatePattern('^https?://[^\s#]+$')]
    [string]$AuthenticodeTimestampUrl,

    [string]$AuthenticodeTool = "signtool.exe",

    [switch]$AllowUnsignedWindowsRelease,

    [ValidateRange(1, [UInt64]::MaxValue)]
    [UInt64]$TrustResetFromGeneration,

    [ValidatePattern('^[A-Za-z0-9_-]{16,128}$')]
    [string]$TrustResetFromReleaseIdentity,

    [string]$WranglerPath = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) "axiusflow-website\node_modules\.bin\wrangler.cmd"),

    [string]$IsccPath = "ISCC.exe",

    [ValidatePattern('^[A-Za-z0-9._-]{1,32}$')]
    [string]$Channel = "stable",

    [ValidatePattern('^[A-Za-z0-9._-]{1,64}$')]
    [string]$RolloutCohort = "all",

    [ValidateRange(0, 100)]
    [byte]$RolloutPercentage = 100,

    [switch]$PackageOnly,

    [ValidatePattern('^[A-Za-z0-9_-]{43}$')]
    [string]$ReleaseVerifyingKey,

    [switch]$PrebuildOnly,

    [switch]$SkipQualification,

    [switch]$SkipBuild,

    [string]$PublisherPath,

    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string]$PublisherSha256
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-RequiredCommandPath([string]$Value, [string]$Description) {
    if (Test-Path -LiteralPath $Value -PathType Leaf) {
        return (Resolve-Path -LiteralPath $Value).Path
    }

    $command = Get-Command $Value -ErrorAction SilentlyContinue
    if ($null -eq $command) {
        throw "$Description is unavailable at '$Value' and was not found on PATH."
    }
    return $command.Source
}

$repo = Split-Path -Parent $PSScriptRoot
$baseUrl = "https://auth.axiusflow.com/releases"
$bucket = "axiusflow-releases"
$wrangler = $null
$trustResetConfigured = (
    $TrustResetFromGeneration -gt 0 -and
    -not [string]::IsNullOrWhiteSpace($TrustResetFromReleaseIdentity)
)
if ($trustResetConfigured -and $Generation -le $TrustResetFromGeneration) {
    throw "Trust-reset publication generation must be newer than its source generation."
}
if (-not $trustResetConfigured -and (
    $TrustResetFromGeneration -gt 0 -or
    -not [string]::IsNullOrWhiteSpace($TrustResetFromReleaseIdentity)
)) {
    throw "Trust reset requires both -TrustResetFromGeneration and -TrustResetFromReleaseIdentity."
}
$authenticodeConfigured = (
    -not [string]::IsNullOrWhiteSpace($AuthenticodeCertificateSha1) -and
    -not [string]::IsNullOrWhiteSpace($AuthenticodeTimestampUrl)
)
if ($AllowUnsignedWindowsRelease -and $authenticodeConfigured) {
    throw "-AllowUnsignedWindowsRelease cannot be combined with Authenticode configuration."
}
if (-not $AllowUnsignedWindowsRelease -and -not $authenticodeConfigured) {
    throw "Windows release packaging requires Authenticode configuration or -AllowUnsignedWindowsRelease."
}
if (-not $authenticodeConfigured -and (
    -not [string]::IsNullOrWhiteSpace($AuthenticodeCertificateSha1) -or
    -not [string]::IsNullOrWhiteSpace($AuthenticodeTimestampUrl)
)) {
    throw "Authenticode certificate and timestamp configuration must be supplied together."
}

if ([string]::IsNullOrWhiteSpace($ReleaseVersion)) {
    $workspaceManifest = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml')
    $inWorkspacePackage = $false
    foreach ($line in $workspaceManifest) {
        if ($line -match '^\s*\[(.+)\]\s*$') {
            $inWorkspacePackage = ($Matches[1] -eq 'workspace.package')
            continue
        }
        if ($inWorkspacePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"\s*$') {
            $ReleaseVersion = $Matches[1]
            break
        }
    }
    if ([string]::IsNullOrWhiteSpace($ReleaseVersion)) {
        throw "Unable to resolve workspace release version from Cargo.toml."
    }
}

if ($PrebuildOnly -and ($PackageOnly -or $SkipQualification -or $SkipBuild -or $PublisherPath -or $PublisherSha256 -or $trustResetConfigured)) {
    throw "-PrebuildOnly cannot be combined with package/publish skip or publisher-path options."
}
if ($PackageOnly -and $SkipBuild) {
    throw "-PackageOnly cannot use -SkipBuild because local packages must rebuild the desktop with self-update disabled."
}
if ($PackageOnly -and (
    -not [string]::IsNullOrWhiteSpace($PublisherPath) -or
    -not [string]::IsNullOrWhiteSpace($PublisherSha256)
)) {
    throw "-PackageOnly must use the repository release publisher so the local-package build contract is current."
}

if (-not $PackageOnly -and -not $PrebuildOnly) {
    $expectedReleaseContext = (
        $env:GITHUB_ACTIONS -eq "true" -and
        $env:GITHUB_WORKFLOW -eq "Production release" -and
        $env:GITHUB_EVENT_NAME -eq "workflow_dispatch" -and
        $env:GITHUB_REF -eq "refs/heads/main" -and
        $env:TRADINGPLOT_RELEASE_ENVIRONMENT -eq "self-hosted-release-station"
    )
    if (-not $expectedReleaseContext) {
        throw "Production publication is CI-authoritative and must run from the self-hosted Production release workflow. Use -PackageOnly for local qualification without R2/channel mutation."
    }
}

$iscc = $null
$authenticodeToolPath = $null
if (-not $PrebuildOnly) {
    $iscc = Resolve-RequiredCommandPath $IsccPath "Inno Setup 6"
    if ($authenticodeConfigured) {
        $authenticodeToolPath = Resolve-RequiredCommandPath $AuthenticodeTool "Windows SignTool"
    }
}
if (-not $PackageOnly -and -not $PrebuildOnly) {
    $wrangler = Resolve-RequiredCommandPath $WranglerPath "Wrangler"
}

if (-not $PrebuildOnly -and -not (Test-Path -LiteralPath $SigningKeyFile -PathType Leaf)) {
    throw "Release signing key is missing at '$SigningKeyFile'. Refusing to create or rotate production trust implicitly."
}

if (-not $PackageOnly -and -not $PrebuildOnly -and -not (Test-Path -LiteralPath $wrangler -PathType Leaf)) {
    throw "Wrangler is unavailable at '$wrangler'. Install website dependencies before publishing."
}

if (-not $PrebuildOnly -and -not (Test-Path -LiteralPath $iscc -PathType Leaf)) {
    throw "Inno Setup 6 is unavailable. Install it or add ISCC.exe to PATH before publishing Windows releases."
}

if (($PrebuildOnly -or $SkipBuild) -and [string]::IsNullOrWhiteSpace($ReleaseVerifyingKey)) {
    throw "Prebuilt release flows require -ReleaseVerifyingKey so binaries and the signing key are bound to the same public trust root."
}

if (-not $PackageOnly -and -not $PrebuildOnly) {
    if (-not $SkipQualification -or -not $SkipBuild -or [string]::IsNullOrWhiteSpace($PublisherPath) -or [string]::IsNullOrWhiteSpace($PublisherSha256)) {
        throw "Production publication requires separately qualified prebuilt binaries and an independently trusted publisher: use -SkipQualification -SkipBuild -PublisherPath -PublisherSha256 with the protected workflow."
    }
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

    $branch = (& git branch --show-current | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $branch -ne "main") {
        throw "Production release requires the main branch."
    }

    $identity = (& git rev-parse HEAD | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $identity -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw "Unable to resolve the release identity from Git HEAD."
    }

    $originMain = (& git rev-parse origin/main | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $originMain -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw "Unable to resolve origin/main. Fetch origin/main before publishing."
    }
    if ($identity -ne $originMain) {
        throw "Production release HEAD must exactly match origin/main."
    }

    if (-not $SkipQualification) {
        Write-Host "Qualifying TradingPlot release $Generation at $identity"
        & cargo fmt --all -- --check
        if ($LASTEXITCODE -ne 0) { throw "cargo fmt gate failed." }

        & cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw "cargo clippy gate failed." }

        & cargo test --workspace --all-features --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo test gate failed." }
    }

    if ($PrebuildOnly) {
        Write-Host "Building release binaries before production signing secrets are materialized"
        $saved = @{}
        foreach ($name in @(
            'TRADINGPLOT_RELEASE_VERIFYING_KEY',
            'TRADINGPLOT_RELEASE_BASE_URL',
            'TRADINGPLOT_BOOTSTRAP_MIN_GENERATION',
            'TRADINGPLOT_RELEASE_IDENTITY',
            'TRADINGPLOT_INSTALL_GENERATION',
            'TRADINGPLOT_AUTHENTICODE_CERT_SHA1',
            'TRADINGPLOT_LOCAL_PACKAGE'
        )) {
            $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        }
        try {
            $env:TRADINGPLOT_RELEASE_VERIFYING_KEY = $ReleaseVerifyingKey
            $env:TRADINGPLOT_RELEASE_BASE_URL = $baseUrl
            $env:TRADINGPLOT_BOOTSTRAP_MIN_GENERATION = [string]$Generation
            $env:TRADINGPLOT_RELEASE_IDENTITY = $identity
            $env:TRADINGPLOT_INSTALL_GENERATION = [string]$Generation
            if ($authenticodeConfigured) {
                $env:TRADINGPLOT_AUTHENTICODE_CERT_SHA1 = $AuthenticodeCertificateSha1
            } else {
                Remove-Item Env:TRADINGPLOT_AUTHENTICODE_CERT_SHA1 -ErrorAction SilentlyContinue
            }
            Remove-Item Env:TRADINGPLOT_LOCAL_PACKAGE -ErrorAction SilentlyContinue

            & cargo build --locked --release -p tradingplot_platform_runtime --bin tradingplot_launcher
            if ($LASTEXITCODE -ne 0) { throw "release launcher prebuild failed." }
            & cargo build --locked --release -p tradingplot_desktop --all-features
            if ($LASTEXITCODE -ne 0) { throw "release desktop prebuild failed." }
        }
        finally {
            foreach ($name in $saved.Keys) {
                if ($null -eq $saved[$name]) {
                    Remove-Item "Env:$name" -ErrorAction SilentlyContinue
                } else {
                    [Environment]::SetEnvironmentVariable($name, $saved[$name])
                }
            }
        }
        Write-Host "Release $Generation qualified and prebuilt without production publication secrets."
        return
    }

    $publishedAt = [DateTime]::UtcNow.ToString(
        "yyyy-MM-dd'T'HH:mm:ss'Z'",
        [Globalization.CultureInfo]::InvariantCulture
    )
    $output = Join-Path $repo ("target\release-publish\{0}-{1}" -f $Generation, $identity)

    $publisherArgs = @(
        "--signing-key-file", $SigningKeyFile,
        "--release-identity", $identity,
        "--generation", $Generation,
        "--release-version", $ReleaseVersion,
        "--minimum-version", $MinimumLauncherVersion,
        "--base-url", $baseUrl,
        "--published-at", $publishedAt,
        "--channel", $Channel,
        "--rollout-cohort", $RolloutCohort,
        "--rollout-percentage", $RolloutPercentage,
        "--output", $output,
        "--iscc", $iscc
    )
    if ($authenticodeConfigured) {
        $publisherArgs += @(
            "--authenticode-tool", $authenticodeToolPath,
            "--authenticode-certificate-sha1", $AuthenticodeCertificateSha1,
            "--authenticode-timestamp-url", $AuthenticodeTimestampUrl
        )
    } else {
        $publisherArgs += "--allow-unsigned-windows-release"
    }
    if ($SkipBuild) {
        $publisherArgs += @("--skip-build", "--expected-verifying-key", $ReleaseVerifyingKey)
    }
    if ($trustResetConfigured) {
        $publisherArgs += @(
            "--trust-reset-from-generation", $TrustResetFromGeneration,
            "--trust-reset-from-release-identity", $TrustResetFromReleaseIdentity
        )
    }
    if (-not $PackageOnly) {
        $publisherArgs += @("--r2-bucket", $bucket, "--wrangler", $wrangler)
        Write-Host "Building, signing, uploading, and verifying immutable release objects"
    } else {
        Write-Host "Building and signing release package without publishing"
    }
    if ([string]::IsNullOrWhiteSpace($PublisherPath)) {
        & cargo run --locked --release -p tradingplot_platform_runtime --bin tradingplot_release_publisher -- @publisherArgs
    } else {
        $resolvedPublisher = Resolve-RequiredCommandPath $PublisherPath "Prebuilt TradingPlot release publisher"
        if (-not [string]::IsNullOrWhiteSpace($PublisherSha256)) {
            $actualPublisherSha256 = (Get-FileHash -LiteralPath $resolvedPublisher -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actualPublisherSha256 -ne $PublisherSha256.ToLowerInvariant()) {
                throw "Trusted release publisher SHA-256 does not match the protected expected digest."
            }
        }
        & $resolvedPublisher @publisherArgs
    }
    if ($LASTEXITCODE -ne 0) {
        if ($PackageOnly) {
            throw "Release qualification/package creation failed."
        }
        throw "Release publication or post-publication verification failed. Inspect the public stable channel before retrying."
    }

    if ($PackageOnly) {
        Write-Host "Release $Generation packaged locally without R2 or channel mutation."
    } else {
        Write-Host "Release $Generation published and public release objects verified."
    }
}
finally {
    Pop-Location
}
