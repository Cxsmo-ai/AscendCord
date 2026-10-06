# Capture resampler response measurement

`resampler-response.png` (7200 × 4800 pixels) and `resampler-response.svg` (vector) plot a measured
sweep through the production stereo resampler in
`crates/discord-voice/src/resample.rs`. The test generates equal-level left/right sinusoids,
converts them to the 48 kHz Opus input rate, skips the startup transient, and uses a least-squares
sine fit to measure fundamental gain and non-tone residual at 481 logarithmic frequency points
(480 intervals, about 1.45% apart). The plotted CSV is emitted by the same
Rust test; each rate/channel/frequency pair is a direct measurement, not a simulated curve.

The sweep covers 20 Hz to the resampler passband edge (up to 20 kHz), for 44.1→48 kHz, native
48→48 kHz bypass, and 96→48 kHz. The gain plot is zoomed to ±0.000010 dB to expose the smallest
measured changes; the acceptance bound is absolute gain below 0.05 dB and fitted residual
below −75 dBc at every plotted point. This isolates the sample-rate conversion code. It does not
include a physical microphone, Windows audio driver, Opus compression, Discord transport, or the
browser decoder, so it is not an end-to-end microphone/VC measurement.

Reproduce on Windows from the repository root:

```powershell
$env:ASCENDCORD_EQ_CSV = (Join-Path (Get-Location) 'docs/audio-response/resampler-measurements.csv')
rustc --edition=2024 --test crates/discord-voice/src/resample.rs -o $env:TEMP\ascendcord-resampler-tests.exe
& $env:TEMP\ascendcord-resampler-tests.exe measured_equal_level_tone_sweep_is_flat_on_both_channels
python docs/audio-response/plot_response.py
```

The extension's receive statistics remain a separate live-call check. Use the opt-in test signal
only for a call you control; the diagnostic tone replaces microphone content while enabled.
