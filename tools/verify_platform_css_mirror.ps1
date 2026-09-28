param(
    [string]$WebsiteCss = (Join-Path $PSScriptRoot "..\..\..\Axiusflow-Org\axiusflow-website\src\styles\platform.css")
)

$ErrorActionPreference = "Stop"
$nativeCss = Join-Path $PSScriptRoot "..\crates\ui\design_system\platform.css"

function Read-ThemeTokens([string]$Path) {
    $text = Get-Content -LiteralPath $Path -Raw
    $text = [regex]::Replace($text, '/\*[\s\S]*?\*/', '')
    $tokens = [ordered]@{}
    foreach ($mode in @("root", "dark")) {
        $selector = if ($mode -eq "root") { ":root" } else { ".dark" }
        $matches = [regex]::Matches($text, [regex]::Escape($selector) + '\s*\{(?<body>[^}]*)\}')
        foreach ($match in $matches) {
            foreach ($declaration in [regex]::Matches($match.Groups['body'].Value, '--(?<name>[a-z0-9-]+)\s*:\s*(?<value>[^;]+);')) {
                $name = "$mode/$($declaration.Groups['name'].Value)"
                $tokens[$name] = $declaration.Groups['value'].Value.Trim().ToLowerInvariant()
            }
        }
    }
    return $tokens
}

$native = Read-ThemeTokens (Resolve-Path -LiteralPath $nativeCss)
$website = Read-ThemeTokens (Resolve-Path -LiteralPath $WebsiteCss)
$sharedNames = $native.Keys | Where-Object { $_ -notlike 'root/font-*' }
$differences = foreach ($name in $sharedNames) {
    if (-not $website.Contains($name) -or $website[$name] -ne $native[$name]) {
        "$name native=$($native[$name]) website=$($website[$name])"
    }
}
if ($differences) {
    throw "Platform token mirror drift detected:`n$($differences -join "`n")"
}
Write-Output "platform_css_mirror=verified tokens=$($sharedNames.Count)"
