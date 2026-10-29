<#
.SYNOPSIS
Builds, signs and (with -Publish) publishes one Windows x64 Aeris release to the release origin.

.DESCRIPTION
Runs on the maintainer's machine, never in CI. It reads the AerisReleases stack outputs, takes
the next install generation from the channel's current pointer, builds the desktop and launcher
with the release configuration (verifying key from the KMS signing key), packages and signs the
release through AWS KMS with aeris_release_packager, builds Aeris-Setup.exe with Inno Setup and
writes the channel pointer. -Publish uploads the immutable release folder first and the channel
pointer last, then invalidates the pointer in CloudFront and reads it back over HTTPS.

The staging channel lives below <base>/staging; production is the base URL itself. Without
-Publish nothing leaves the machine. Requires PowerShell 7, an AWS CLI session (aws login),
Inno Setup 6.6 or newer, the licensed Rithmic kit behind rithmic_proto_kit, and a clean
worktree (pushed to origin/main for production).

.EXAMPLE
pwsh -File tools/release/publish_update.ps1 -Channel staging -Publish
#>
[CmdletBinding()]
param(
    [ValidateSet('staging', 'production')]
    [string]$Channel = 'staging',
    [switch]$Publish,
    [ValidateRange(1, [long]::MaxValue)]
    [long]$BootstrapMinimumGeneration = 1,
    [string]$WorkRoot = (Join-Path $env:LOCALAPPDATA 'Aeris-Release'),
    [string]$InnoSetupCompiler = (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

$region = 'us-east-1'
$stackName = 'AerisReleases'
$platform = 'windows'
$architecture = 'x86_64'
$immutable = 'public, max-age=31536000, immutable'
$pointerCache = 'public, max-age=0, s-maxage=60, must-revalidate'
$repository = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$releaseVariables = @(
    'AERIS_RELEASE_VERIFYING_KEY', 'AERIS_RELEASE_BASE_URL', 'AERIS_BOOTSTRAP_MIN_GENERATION',
    'AERIS_RELEASE_IDENTITY', 'AERIS_INSTALL_GENERATION', 'AERIS_AUTHENTICODE_CERT_SHA1'
)

function Invoke-Native {
    param([string]$FilePath, [string[]]$Arguments)
    $output = & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$FilePath $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
    $output
}

function Invoke-Aws {
    param([string[]]$Arguments)
    Invoke-Native 'aws' ($Arguments + @('--region', $region))
}

function Test-S3Prefix {
    param([string]$Bucket, [string]$Key)
    $found = Invoke-Aws @('s3api', 'list-objects-v2', '--bucket', $Bucket, '--prefix', $Key,
        '--max-items', '1', '--query', 'Contents[0].Key', '--output', 'text')
    ($found | Out-String).Trim() -notin @('', 'None')
}

if (-not $IsWindows) { throw 'Windows releases are built on Windows.' }
foreach ($name in $releaseVariables + 'AERIS_RITHMIC_KIT_DISABLED') {
    if (Test-Path "env:$name") { throw "$name is set in this shell; open a clean shell." }
}
if (-not (Test-Path -LiteralPath $InnoSetupCompiler)) { throw "Inno Setup is not at $InnoSetupCompiler" }
if (-not (Test-Path -LiteralPath (Join-Path $repository 'rithmic_proto_kit\proto\change_log'))) {
    throw 'The licensed Rithmic kit is not reachable through rithmic_proto_kit\proto.'
}
if (Get-Process -Name cargo, rustc -ErrorAction SilentlyContinue) {
    throw 'Another Cargo build is running; cargo clean must not run under it.'
}

$status = Invoke-Native 'git' @('-C', $repository, 'status', '--porcelain')
if ($status) { throw 'The worktree has uncommitted changes; a release is built from one commit.' }
$commit = (Invoke-Native 'git' @('-C', $repository, 'rev-parse', 'HEAD') | Out-String).Trim()
if ($Channel -eq 'production') {
    Invoke-Native 'git' @('-C', $repository, 'fetch', '--quiet', 'origin', 'main') | Out-Null
    $published = (Invoke-Native 'git' @('-C', $repository, 'rev-parse', 'origin/main') | Out-String).Trim()
    if ($commit -ne $published) { throw 'Production releases are built only from the pushed origin/main.' }
}

$workspace = Get-Content -Raw -LiteralPath (Join-Path $repository 'Cargo.toml')
$versionMatch = [regex]::Match($workspace, '(?ms)^\[workspace\.package\].*?^version\s*=\s*"(\d+\.\d+\.\d+)"')
if (-not $versionMatch.Success) { throw 'Cargo.toml has no [workspace.package] version.' }
$version = $versionMatch.Groups[1].Value

Invoke-Aws @('sts', 'get-caller-identity', '--output', 'json') | Out-Null
$outputs = @{}
(Invoke-Aws @('cloudformation', 'describe-stacks', '--stack-name', $stackName, '--output', 'json') |
    Out-String | ConvertFrom-Json).Stacks[0].Outputs |
    ForEach-Object { $outputs[$_.OutputKey] = $_.OutputValue }
$bucket = $outputs['BucketName']
$prefix = if ($Channel -eq 'staging') { 'staging/' } else { '' }
$baseUrl = $outputs['ReleaseBaseUrl'] + $(if ($Channel -eq 'staging') { '/staging' } else { '' })
$kmsKey = $outputs['SigningKeyArn']
$pointerKey = "${prefix}channels/stable/$platform/$architecture.json"

$channelRoot = Join-Path $WorkRoot $Channel
$origin = Join-Path $channelRoot 'origin'
New-Item -ItemType Directory -Force -Path $origin | Out-Null

$previous = $null
$generation = 1
if (Test-S3Prefix $bucket $pointerKey) {
    $current = Invoke-Aws @('s3', 'cp', "s3://$bucket/$pointerKey", '-') | Out-String | ConvertFrom-Json
    $generation = [long]$current.generation + 1
    $previousPath = "$platform/$architecture/$($current.generation)-$($current.release_identity)"
    $previous = Join-Path $origin ($previousPath -replace '/', '\')
    if (-not (Test-Path -LiteralPath (Join-Path $previous 'manifest.json'))) {
        Write-Host "Downloading the current release $previousPath for the block plan"
        Invoke-Aws @('s3', 'cp', "s3://$bucket/$prefix$previousPath/", $previous, '--recursive',
            '--only-show-errors') | Out-Null
    }
}
$identity = "aeris-$($version -replace '\.', '-')-g$generation-$($commit.Substring(0, 12))"
$releasePath = "$platform/$architecture/$generation-$identity"
$releaseDirectory = Join-Path $origin ($releasePath -replace '/', '\')
if (Test-S3Prefix $bucket "$prefix$releasePath/") { throw "$releasePath is already published." }
if (Test-Path -LiteralPath $releaseDirectory) {
    # Never published (checked above), so it is a stale local attempt at this generation.
    Remove-Item -LiteralPath $releaseDirectory -Recurse -Force
}
Write-Host "Building $Channel release $version, generation $generation ($identity)"

Push-Location $repository
try {
    Invoke-Native 'cargo' @('clean') | Out-Null
    # The packager is a development build: it must not carry a release configuration.
    Invoke-Native 'cargo' @('build', '--release', '--locked', '-p', 'aeris_release_packager',
        '--bin', 'aeris_release_packager') | Out-Null
    $tools = Join-Path $channelRoot 'tools'
    New-Item -ItemType Directory -Force -Path $tools | Out-Null
    $packager = Join-Path $tools 'aeris_release_packager.exe'
    Copy-Item -LiteralPath (Join-Path $repository 'target\release\aeris_release_packager.exe') $packager -Force

    $verifyingKey = (Invoke-Native $packager @('verifying-key', '--kms-key', $kmsKey) |
        Out-String | ConvertFrom-Json).verifying_key

    $env:AERIS_RELEASE_VERIFYING_KEY = $verifyingKey
    $env:AERIS_RELEASE_BASE_URL = $baseUrl
    $env:AERIS_BOOTSTRAP_MIN_GENERATION = "$BootstrapMinimumGeneration"
    $env:AERIS_RELEASE_IDENTITY = $identity
    $env:AERIS_INSTALL_GENERATION = "$generation"
    try {
        Invoke-Native 'cargo' @('build', '--release', '--locked', '-p', 'aeris_desktop',
            '--bin', 'aeris_desktop', '-p', 'aeris_release_lifecycle', '--bin', 'aeris_launcher') | Out-Null
    }
    finally {
        foreach ($name in $releaseVariables) { Remove-Item "env:$name" -ErrorAction SilentlyContinue }
    }
    # A missing kit falls back to a kitless build silently; a release must carry it.
    if (-not (Get-ChildItem -Path (Join-Path $repository 'target\release\build') -Directory `
                -Filter 'aeris_rithmic_protocol_adapter-*' |
            Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'out\rithmic.protobuf.rs') })) {
        throw 'The Rithmic kit was not compiled into the release.'
    }
}
finally {
    Pop-Location
}

$payload = Join-Path $channelRoot "payload-$generation"
if (Test-Path -LiteralPath $payload) { Remove-Item -LiteralPath $payload -Recurse -Force }
New-Item -ItemType Directory -Path $payload | Out-Null
foreach ($name in 'aeris_desktop.exe', 'aeris_launcher.exe') {
    Copy-Item -LiteralPath (Join-Path $repository "target\release\$name") (Join-Path $payload $name)
}

$identityReport = Join-Path $channelRoot "launcher-identity-$generation.json"
$probe = Start-Process -FilePath (Join-Path $payload 'aeris_launcher.exe') -ArgumentList '--launcher-identity' `
    -RedirectStandardOutput $identityReport -NoNewWindow -Wait -PassThru
if ($probe.ExitCode -ne 0) { throw "The built launcher's identity check exited with $($probe.ExitCode)." }
$embedded = Get-Content -Raw -LiteralPath $identityReport | ConvertFrom-Json
if ($embedded.release_identity -ne $identity -or [long]$embedded.install_generation -ne $generation) {
    throw 'The built launcher does not carry this release identity and generation.'
}

$packageArguments = @('release', '--kms-key', $kmsKey, '--payload', $payload, '--output', $origin,
    '--base-url', $baseUrl, '--release-identity', $identity, '--generation', "$generation",
    '--inno-script', (Join-Path $repository 'tools\windows\aeris_setup.iss'),
    '--icon', (Join-Path $repository 'apps\desktop\assets\aeris_assets\icons\desktop\windows\aeris.ico'),
    '--version', $version)
if ($previous) { $packageArguments += @('--previous', $previous) }
$report = Invoke-Native $packager $packageArguments | Out-String | ConvertFrom-Json
Write-Host "Packaged $($report.release_directory); block plan: $($report.block_plan | ConvertTo-Json -Compress)"
Invoke-Native $InnoSetupCompiler ([string[]]$report.inno_setup_arguments) | Out-Null

$publishedAt = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
$pointer = (Invoke-Native $packager @('channel', '--output', $origin, '--base-url', $baseUrl,
        '--release-identity', $identity, '--generation', "$generation", '--version', $version,
        '--published-at', $publishedAt) | Out-String | ConvertFrom-Json).channel_pointer
Write-Host "Channel pointer written to $pointer"

if (-not $Publish) {
    Write-Host 'Packaged only; rerun with -Publish to upload. Nothing left this machine.'
    return
}

# The release folder is immutable and goes first; the pointer that names it goes last.
Invoke-Aws @('s3', 'cp', $releaseDirectory, "s3://$bucket/$prefix$releasePath/", '--recursive',
    '--cache-control', $immutable, '--only-show-errors') | Out-Null
$manifestUrl = "$baseUrl/$releasePath/manifest.json"
Invoke-Native 'curl.exe' @('--fail', '--silent', '--show-error', '--head', '--output', 'NUL', $manifestUrl) | Out-Null
Invoke-Aws @('s3', 'cp', $pointer, "s3://$bucket/$pointerKey", '--content-type', 'application/json',
    '--cache-control', $pointerCache, '--only-show-errors') | Out-Null
$invalidation = Invoke-Aws @('cloudfront', 'create-invalidation', '--distribution-id',
    $outputs['DistributionId'], '--paths', "/$pointerKey", '--output', 'json') | Out-String | ConvertFrom-Json
Invoke-Aws @('cloudfront', 'wait', 'invalidation-completed', '--distribution-id', $outputs['DistributionId'],
    '--id', $invalidation.Invalidation.Id) | Out-Null

$live = Invoke-Native 'curl.exe' @('--fail', '--silent', '--show-error', "$baseUrl/channels/stable/$platform/$architecture.json") |
    Out-String | ConvertFrom-Json
if ($live.release_identity -ne $identity -or [long]$live.generation -ne $generation) {
    throw "The live channel pointer does not name $identity yet."
}
Write-Host "Published $Channel $version generation $generation ($identity) at $baseUrl"
