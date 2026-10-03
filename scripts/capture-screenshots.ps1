# Captures the README screenshots from the built-in demo scenes (synthetic data only).
#
#   cargo build --release -p ascendcord --features demo
#   ./scripts/capture-screenshots.ps1 [-Executable path\to\ascendcord.exe] [-Size 1440x900]
#
# Each scene runs in a fresh process that saves one PNG and exits.
param(
    [string] $Executable = 'target\release\ascendcord.exe',
    [string] $OutputDir = 'docs\screenshots',
    [string] $Size = '1440x900'
)
$ErrorActionPreference = 'Stop'

$scenes = [ordered]@{
    'chat'            = @('--demo-chat')
    'friends'         = @('--demo-friends')
    'voice-call'      = @('--demo-call')
    'screen-share'    = @('--demo-call', '--demo-screen-share')
    'settings'        = @('--demo-settings=appearance')
    'themes'          = @('--demo-settings=themes')
    'server-settings' = @('--demo-server-settings')
    'threads'         = @('--demo-threads')
    'search'          = @('--demo-search=history')
    'profile'         = @('--demo-profile')
    'emoji'           = @('--demo-emoji')
    'light'           = @('--demo-chat', '--demo-light')
}

if (-not (Test-Path -LiteralPath $Executable)) {
    throw "Missing $Executable. Build it with: cargo build --release -p ascendcord --features demo"
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$failed = @()
foreach ($name in $scenes.Keys) {
    $target = [IO.Path]::GetFullPath((Join-Path $OutputDir "$name.png"))
    Remove-Item -LiteralPath $target -ErrorAction SilentlyContinue
    # Start-Process joins arguments with spaces, so a path with spaces must be quoted.
    $arguments = @('--demo') + $scenes[$name] + @("--screenshot-to=`"$target`"", "--screenshot-size=$Size")
    $process = Start-Process -FilePath $Executable -ArgumentList $arguments -PassThru -Wait -WindowStyle Normal
    if ($process.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $target)) {
        Write-Warning "$name failed (exit $($process.ExitCode))"
        $failed += $name
    } else {
        Write-Output ("{0,-16} {1:N0} KB" -f $name, ((Get-Item $target).Length / 1KB))
    }
}
if ($failed) { throw "Scenes failed: $($failed -join ', ')" }
