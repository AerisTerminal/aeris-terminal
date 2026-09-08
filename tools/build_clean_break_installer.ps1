param(
    [string]$OutputDirectory = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$isccCommand = Get-Command ISCC.exe -ErrorAction SilentlyContinue
$iscc = if ($null -ne $isccCommand) {
    $isccCommand.Source
} else {
    Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"
}
if (-not (Test-Path -LiteralPath $iscc -PathType Leaf)) {
    throw "Inno Setup 6 is unavailable."
}

Push-Location $repo
try {
    & cargo build --locked --release -p axiusflow_desktop
    if ($LASTEXITCODE -ne 0) {
        throw "Axiusflow desktop release build failed."
    }

    $identity = (& git rev-parse --short=12 HEAD | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $identity -notmatch '^[0-9a-fA-F]{7,12}$') {
        throw "Unable to resolve the Git release identity."
    }

    if (-not $OutputDirectory) {
        $OutputDirectory = Join-Path $repo "target\clean-break-release"
    }
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    New-Item -ItemType Directory -Force -Path $output | Out-Null

    $desktop = Join-Path $repo "target\release\axiusflow_desktop.exe"
    $icon = Join-Path $repo "apps\desktop\assets\brand_assets\axiusflow.ico"
    $script = Join-Path $repo "tools\windows\axiusflow_clean_break_setup.iss"
    foreach ($inputPath in @($desktop, $icon, $script)) {
        if (-not (Test-Path -LiteralPath $inputPath -PathType Leaf)) {
            throw "Installer input is unavailable: $inputPath"
        }
    }

    $setup = Join-Path $output "Axiusflow-Setup.exe"
    Remove-Item -LiteralPath $setup -Force -ErrorAction SilentlyContinue
    $version = "0.1.0-$identity"

    & $iscc `
        "/Qp" `
        "/DAppVersion=$version" `
        "/DDesktopPath=$desktop" `
        "/DIconPath=$icon" `
        "/DOutputDir=$output" `
        $script
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compiler failed."
    }
    if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
        throw "Inno Setup did not emit Axiusflow-Setup.exe."
    }

    $hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash
    $size = (Get-Item -LiteralPath $setup).Length
    Write-Output "installer=$setup"
    Write-Output "sha256=$hash"
    Write-Output "size=$size"
    Write-Output "release_identity=$identity"
}
finally {
    Pop-Location
}
