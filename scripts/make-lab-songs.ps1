<#
.SYNOPSIS
Cuts short clips from music files into the folder format `--test-sweep-music` reads.

.DESCRIPTION
Writes NN.s16 (48 kHz, stereo, 16-bit little-endian, raw) and manifest.tsv (NN<TAB>title)
into -Out. Clips are made with ffmpeg's soxr resampler. Keep the folder out of the
repository: it holds pieces of your own music.

.EXAMPLE
pwsh scripts\make-lab-songs.ps1 -Out E:\lab-songs -Start 60 -Seconds 3 -Files "D:\Music\a.flac","D:\Music\b.flac"
#>
param(
	[Parameter(Mandatory)] [string]$Out,
	[Parameter(Mandatory)] [string[]]$Files,
	[double]$Start = 60,
	[double]$Seconds = 3
)
$ErrorActionPreference = "Stop"
if ($Files.Count -gt 16) { throw "AscendCord plays at most 16 clips." }
if ($Seconds -gt 10) { throw "AscendCord plays at most 10 s of each clip." }
New-Item -ItemType Directory -Force $Out | Out-Null
$manifest = @()
for ($i = 0; $i -lt $Files.Count; $i++) {
	$id = "{0:D2}" -f ($i + 1)
	$file = (Resolve-Path $Files[$i]).Path
	& ffmpeg -hide_banner -loglevel error -y -ss $Start -t $Seconds -i $file `
		-af "aresample=resampler=soxr:precision=28:osr=48000" -ac 2 -ar 48000 -f s16le -acodec pcm_s16le `
		(Join-Path $Out "$id.s16")
	if ($LASTEXITCODE) { throw "ffmpeg failed on $file" }
	$manifest += "$id`t$([IO.Path]::GetFileNameWithoutExtension($file))"
}
[IO.File]::WriteAllLines((Join-Path $Out "manifest.tsv"), $manifest)
Write-Host "Wrote $($Files.Count) clips to $Out"
