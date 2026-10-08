<#
.SYNOPSIS
Runs one AscendCord audio lab test without anyone at the keyboard and prints its report.

.DESCRIPTION
Starts ascendcord.exe in sweep mode in a test voice channel. The AscendCord Stereo Proof
extension, in a browser where a Discord account other than AscendCord's is signed in and a
tab is open on that channel, joins the call by itself, measures both directions and sends
the finished run to AscendCord, which writes voice-bridge\lab-report.json. This script waits
for that file, stops AscendCord and prints the main figures.

Needs, once: the extension loaded unpacked in Edge; one Discord tab of the second account open
on https://discord.com/channels/<guild>/<channel> (or pass -Guild to open it).

.EXAMPLE
pwsh scripts\audio-lab.ps1 -Exe .\ascendcord.exe -Channel 1034272505839497270 -Music E:\lab-songs
#>
param(
	[Parameter(Mandatory)] [string]$Exe,
	[Parameter(Mandatory)] [string]$Channel,
	[string]$Guild,
	[string]$Music,
	[int]$TimeoutMinutes = 12,
	[switch]$ReloadExtension,
	[string]$ExtensionId = "jbchdifpgimmmmlnimidbfockpbigfni"
)
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$bridge = Join-Path $env:LOCALAPPDATA "AscendCord\voice-bridge"
$report = Join-Path $bridge "lab-report.json"
$exePath = (Resolve-Path $Exe).Path

if ($ReloadExtension) {
	& (Join-Path $PSScriptRoot "edge-reload-extension.ps1") -Id $ExtensionId
}

Get-Process ascendcord -ErrorAction SilentlyContinue | Stop-Process
Start-Sleep -Seconds 3
$started = Get-Date
$arguments = @("--test-sweep-channel=$Channel")
if ($Music) { $arguments += "`"--test-sweep-music=$((Resolve-Path $Music).Path)`"" }
Start-Process -FilePath $exePath -ArgumentList $arguments -WorkingDirectory (Split-Path $exePath)
if ($Guild) {
	Start-Sleep -Seconds 5
	Start-Process "msedge.exe" "https://discord.com/channels/$Guild/$Channel"
}

Write-Host "Waiting for the lab report (up to $TimeoutMinutes minutes)..."
$deadline = $started.AddMinutes($TimeoutMinutes)
while ((Get-Date) -lt $deadline) {
	Start-Sleep -Seconds 10
	if ((Test-Path $report) -and (Get-Item $report).LastWriteTime -gt $started) { break }
	$status = Get-Content (Join-Path $bridge "status.json") -Raw -ErrorAction SilentlyContinue | ConvertFrom-Json -ErrorAction SilentlyContinue
	if ($status) { Write-Host ("  {0:HH:mm:ss} {1} {2}" -f (Get-Date), $status.call_phase, $status.status) }
}
Get-Process ascendcord -ErrorAction SilentlyContinue | Stop-Process
if (-not ((Test-Path $report) -and (Get-Item $report).LastWriteTime -gt $started)) {
	Write-Error "No lab report within $TimeoutMinutes minutes. Is the Discord tab open on the channel?"
	exit 1
}

$run = Get-Content $report -Raw | ConvertFrom-Json
$forward = $run.measurement_lab
$fmt = { param($value, $unit = " dB") if ($null -eq $value) { "-" } else { "{0:N1}{1}" -f [double]$value, $unit } }
Write-Host ""
Write-Host "Run $($run.id)"
Write-Host ("Forward (AscendCord -> Discord -> browser): {0} passes, {1}/48 bands, ripple {2}, THD+N {3}, separation {4}, stereo {5}" -f `
	$forward.passes, $forward.summary.measured_response_bands, (& $fmt $forward.summary.ripple_100_16k_db),
	(& $fmt $forward.summary.median_thdn_db), (& $fmt $forward.summary.median_separation_db), $forward.summary.stereo_preserved)
Write-Host ("  level steps (dB from ideal): {0}" -f (($forward.linearity.gain_db | ForEach-Object { & $fmt $_ "" }) -join ", "))
if ($forward.content) {
	$sections = $forward.content.sections.PSObject.Properties | ForEach-Object { "{0} {1}" -f $_.Name, (& $fmt $_.Value.srr_db) }
	Write-Host ("  null test (signal to residue): {0}" -f ($sections -join " | "))
	Write-Host ("  pre-echo {0}, delay {1} samples, slips {2}, dropouts {3}" -f (& $fmt $forward.content.pre_echo_db.median_db),
		(& $fmt $forward.content.delay_samples ""), $forward.content.slipped_blocks, $forward.content.dropout_blocks)
}
if ($run.return_lab) {
	Write-Host ("Return (browser -> Discord -> AscendCord): {0} passes, {1}/48 bands, ripple {2}, THD+N {3}" -f `
		$run.return_lab.passes, $run.return_lab.summary.measured_response_bands,
		(& $fmt $run.return_lab.summary.ripple_100_16k_db), (& $fmt $run.return_lab.summary.median_thdn_db))
}
if ($run.sender_health) {
	Write-Host ("AscendCord sending: largest gap {0} ms, stalls {1}/s, capture drops {2}" -f $run.sender_health.max_send_gap_ms,
		$run.sender_health.transport_loop_stalls_per_second, $run.sender_health.capture_ring_drops)
}
if ($run.network) {
	Write-Host ("Network: peak loss {0} %, concealment events {1}, stretched samples {2}" -f $run.network.peak_loss_percent,
		$run.network.concealment_events, $run.network.time_stretched_samples)
}
Write-Host "Full report: $report"
$ok = $forward.passes -ge 2 -and $forward.summary.measured_response_bands -ge 46 -and $forward.summary.stereo_preserved
exit ([int](-not $ok))
