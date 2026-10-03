param(
    [int]$MinimumFreeGiB = 3
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$buildRoot = Join-Path $repoRoot '.build-check'
$buildTemp = Join-Path $buildRoot 'temp'
$targetDir = Join-Path $buildRoot 'target'
$builtExe = Join-Path $targetDir 'x86_64-pc-windows-msvc\debug\ascendcord.exe'
$stableExe = Join-Path $buildRoot 'ascendcord-final.exe'
$stagedExe = "$stableExe.new"
$volumeRoot = [IO.Path]::GetPathRoot($repoRoot)
$driveName = $volumeRoot.TrimEnd('\').TrimEnd(':')

[IO.Directory]::CreateDirectory($buildRoot) | Out-Null
[IO.Directory]::CreateDirectory($buildTemp) | Out-Null

function Invoke-TargetCleanup {
    & cargo clean --manifest-path (Join-Path $repoRoot 'Cargo.toml') --target-dir $targetDir
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "Cargo target cleanup exited with code $LASTEXITCODE."
    }
}

try {
    # Remove only Cargo's generated build directory before measuring headroom.
    Invoke-TargetCleanup

    $drive = Get-PSDrive -Name $driveName
    $freeGiB = [math]::Floor($drive.Free / 1GB)
    if ($freeGiB -lt $MinimumFreeGiB) {
        throw "Only $freeGiB GiB are free on $volumeRoot; at least $MinimumFreeGiB GiB are required for this build."
    }

	Push-Location $repoRoot
	$previousIncremental = $env:CARGO_INCREMENTAL
	$previousDevDebug = $env:CARGO_PROFILE_DEV_DEBUG
	$previousTemp = $env:TEMP
	$previousTmp = $env:TMP
	$previousLibclang = $env:LIBCLANG_PATH
	try {
		$env:CARGO_INCREMENTAL = '0'
		$env:CARGO_PROFILE_DEV_DEBUG = '0'
		# miniaudio's bindings are checked in (vendor/ep-miniaudio-sys/bindings), so
		# libclang is optional: it is only used when MINIAUDIO_REGENERATE_BINDINGS is set.
		$libclangCandidates = @(
			$env:LIBCLANG_PATH,
			(Join-Path $env:LOCALAPPDATA 'Temp\tesktop-libclang12\clang\native'),
			'C:\Program Files\LLVM\bin'
		) | Where-Object { $_ }
		$libclangPath = $libclangCandidates |
			Where-Object { Test-Path -LiteralPath (Join-Path $_ 'libclang.dll') } |
			Select-Object -First 1
		if ($libclangPath) {
			$env:LIBCLANG_PATH = $libclangPath
		}
		# MSVC's linker writes scratch files to TEMP/TMP. Keep those writes on
		# the workspace volume so a full system drive cannot break a valid build.
		$env:TEMP = $buildTemp
		$env:TMP = $buildTemp
		& cargo build --locked -p ascendcord --bin ascendcord `
			--target x86_64-pc-windows-msvc `
			--target-dir $targetDir `
			--jobs 2
		if ($LASTEXITCODE -ne 0) {
			throw "Cargo build failed with exit code $LASTEXITCODE."
		}
	}
	finally {
		$env:CARGO_INCREMENTAL = $previousIncremental
		$env:CARGO_PROFILE_DEV_DEBUG = $previousDevDebug
		$env:TEMP = $previousTemp
		$env:TMP = $previousTmp
		$env:LIBCLANG_PATH = $previousLibclang
		Pop-Location
	}

    if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
        throw "Build succeeded but the expected executable was not produced: $builtExe"
    }

    # Keep the previous launchable binary intact if compilation fails. Close it
    # only after the new binary has been built and is ready to replace it.
    $running = Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($stableExe)) -ErrorAction SilentlyContinue
    foreach ($process in $running) {
        [void]$process.CloseMainWindow()
        if (-not $process.WaitForExit(10000)) {
            throw 'The running app did not close normally; the previous launch binary was preserved.'
        }
    }

    Copy-Item -LiteralPath $builtExe -Destination $stagedExe -Force
    $builtHash = (Get-FileHash -LiteralPath $builtExe -Algorithm SHA256).Hash
    $stagedHash = (Get-FileHash -LiteralPath $stagedExe -Algorithm SHA256).Hash
    if ($builtHash -ne $stagedHash) {
        throw 'The staged executable did not match the compiled binary; the previous launch binary was preserved.'
    }

    Move-Item -LiteralPath $stagedExe -Destination $stableExe -Force
    Write-Host "Launchable executable: $stableExe"
    Write-Host "SHA-256: $stagedHash"
}
finally {
    # Keep the launchable copy; discard only reproducible Cargo intermediates.
    Invoke-TargetCleanup
    $drive = Get-PSDrive -Name $driveName
    Write-Host ("Free space after cleanup: {0:N2} GiB" -f ($drive.Free / 1GB))
}
