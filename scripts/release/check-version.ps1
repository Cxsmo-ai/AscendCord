# Fails unless the pushed tag is v<version> for the workspace version in Cargo.toml.
param([Parameter(Mandatory = $true)][string] $Tag)
$ErrorActionPreference = 'Stop'

$manifest = Get-Content -Raw -LiteralPath 'Cargo.toml'
if ($manifest -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
    throw 'No [workspace.package] version in Cargo.toml'
}
$version = $Matches[1]
if ($Tag -ne "v$version") {
    throw "Tag $Tag does not match the crate version v$version"
}
Write-Output "Releasing AscendCord $version"
