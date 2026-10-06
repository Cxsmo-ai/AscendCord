# Turns `cargo xtask package` output into the release assets the in-app updater expects:
#   ascendcord-<tag>-Windows-X64.zip         portable build (also used by in-app updates)
#   ascendcord-<tag>-Windows-X64-Setup.exe   per-user installer
#   SHA256SUMS.txt                           checksums for every asset
param([Parameter(Mandatory = $true)][string] $Tag)
$ErrorActionPreference = 'Stop'

$version = $Tag.TrimStart('v')
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'ARM64' } else { 'X64' }
$out = Join-Path $PWD 'release-assets'
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $out | Out-Null

if (-not (Test-Path 'dist\ascendcord.exe')) { throw 'dist\ascendcord.exe is missing; run cargo xtask package first' }
$zip = Join-Path $out "ascendcord-$Tag-Windows-$arch.zip"
Compress-Archive -Path 'dist\*' -DestinationPath $zip -CompressionLevel Optimal

# The unpacked browser extension is a separate, browser-loadable release asset.
$extension = Join-Path $out "ascendcord-$Tag-Stereo-Proof-Extension.zip"
Compress-Archive -Path 'browser-extension\stereo-proof\*' -DestinationPath $extension -CompressionLevel Optimal

$installer = "dist-installer\ascendcord-$version-setup.exe"
if (Test-Path $installer) {
    Copy-Item $installer (Join-Path $out "ascendcord-$Tag-Windows-$arch-Setup.exe")
} else {
    throw "Installer $installer was not built (is NSIS installed?)"
}

# Arch: the package and PKGBUILD built and verified by the release's Arch job.
if (Test-Path 'arch-out') {
    Copy-Item 'arch-out\*' $out
}
# Otherwise a PKGBUILD pinned to this tag's source archive, with its checksum filled in.
if ($env:GITHUB_REPOSITORY -and -not (Test-Path (Join-Path $out 'PKGBUILD'))) {
    $source = Join-Path $env:RUNNER_TEMP 'source.tar.gz'
    Invoke-WebRequest "https://github.com/$env:GITHUB_REPOSITORY/archive/refs/tags/$Tag.tar.gz" -OutFile $source
    $sum = (Get-FileHash -Algorithm SHA256 -LiteralPath $source).Hash.ToLowerInvariant()
    $pkgbuild = (Get-Content -Raw 'packaging\aur\PKGBUILD') `
        -replace '(?m)^pkgver=.*$', "pkgver=$version" `
        -replace "sha256sums=\('SKIP'\)", "sha256sums=('$sum')"
    if ($pkgbuild -match "'SKIP'") { throw 'The release PKGBUILD still skips checksum verification' }
    $pkgbuild = $pkgbuild -replace "`r`n", "`n"
    [IO.File]::WriteAllText((Join-Path $out 'PKGBUILD'), $pkgbuild, [Text.UTF8Encoding]::new($false))
}

$lines = Get-ChildItem $out -File | Sort-Object Name | ForEach-Object {
    "$((Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant())  $($_.Name)"
}
# LF line endings so `sha256sum -c` works on Linux too.
[IO.File]::WriteAllText((Join-Path $out 'SHA256SUMS.txt'), (($lines -join "`n") + "`n"), [Text.UTF8Encoding]::new($false))
Get-ChildItem $out | Format-Table Name, Length
