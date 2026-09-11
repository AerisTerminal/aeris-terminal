param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [UInt64]::MaxValue)]
    [UInt64]$Generation,

    [string]$ReleaseVersion,

    [string]$MinimumLauncherVersion = "0.2.0",

    [string]$SigningKeyFile = (Join-Path $env:LOCALAPPDATA "Axiusflow\release-signing-key.b64"),

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$AuthenticodeCertificateSha1,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^https?://[^\s#]+$')]
    [string]$AuthenticodeTimestampUrl,

    [string]$AuthenticodeTool = "signtool.exe",

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

if ($PrebuildOnly -and ($PackageOnly -or $SkipQualification -or $SkipBuild -or $PublisherPath -or $PublisherSha256)) {
    throw "-PrebuildOnly cannot be combined with package/publish skip or publisher-path options."
}

if (-not $PackageOnly -and -not $PrebuildOnly) {
    $expectedReleaseContext = (
        $env:GITHUB_ACTIONS -eq "true" -and
        $env:GITHUB_WORKFLOW -eq "Production release" -and
        $env:GITHUB_EVENT_NAME -eq "workflow_dispatch" -and
        $env:GITHUB_REF -eq "refs/heads/main" -and
        $env:AXIUSFLOW_RELEASE_ENVIRONMENT -eq "production-release"
    )
    if (-not $expectedReleaseContext) {
        throw "Production publication is CI-authoritative and must run from the protected Production release workflow. Use -PackageOnly for local qualification without R2/channel mutation."
    }
}

$iscc = $null
$authenticodeToolPath = $null
if (-not $PrebuildOnly) {
    $iscc = Resolve-RequiredCommandPath $IsccPath "Inno Setup 6"
    $authenticodeToolPath = Resolve-RequiredCommandPath $AuthenticodeTool "Windows SignTool"
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
        Write-Host "Qualifying Axiusflow release $Generation at $identity"
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
            'AXIUSFLOW_RELEASE_VERIFYING_KEY',
            'AXIUSFLOW_RELEASE_BASE_URL',
            'AXIUSFLOW_BOOTSTRAP_MIN_GENERATION',
            'AXIUSFLOW_RELEASE_IDENTITY',
            'AXIUSFLOW_INSTALL_GENERATION',
            'AXIUSFLOW_AUTHENTICODE_CERT_SHA1'
        )) {
            $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        }
        try {
            $env:AXIUSFLOW_RELEASE_VERIFYING_KEY = $ReleaseVerifyingKey
            $env:AXIUSFLOW_RELEASE_BASE_URL = $baseUrl
            $env:AXIUSFLOW_BOOTSTRAP_MIN_GENERATION = [string]$Generation
            $env:AXIUSFLOW_RELEASE_IDENTITY = $identity
            $env:AXIUSFLOW_INSTALL_GENERATION = [string]$Generation
            $env:AXIUSFLOW_AUTHENTICODE_CERT_SHA1 = $AuthenticodeCertificateSha1

            & cargo build --locked --release -p axiusflow_platform_runtime --bin axiusflow_launcher
            if ($LASTEXITCODE -ne 0) { throw "release launcher prebuild failed." }
            & cargo build --locked --release -p axiusflow_desktop --all-features
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
        "--iscc", $iscc,
        "--authenticode-tool", $authenticodeToolPath,
        "--authenticode-certificate-sha1", $AuthenticodeCertificateSha1,
        "--authenticode-timestamp-url", $AuthenticodeTimestampUrl
    )
    if ($SkipBuild) {
        $publisherArgs += @("--skip-build", "--expected-verifying-key", $ReleaseVerifyingKey)
    }
    if (-not $PackageOnly) {
        $publisherArgs += @("--r2-bucket", $bucket, "--wrangler", $wrangler)
        Write-Host "Building, signing, uploading, and verifying immutable release objects"
    } else {
        Write-Host "Building and signing release package without publishing"
    }
    if ([string]::IsNullOrWhiteSpace($PublisherPath)) {
        & cargo run --locked --release -p axiusflow_platform_runtime --bin axiusflow_release_publisher -- @publisherArgs
    } else {
        $resolvedPublisher = Resolve-RequiredCommandPath $PublisherPath "Prebuilt Axiusflow release publisher"
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
