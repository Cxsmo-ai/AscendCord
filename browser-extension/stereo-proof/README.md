# AscendCord Stereo Proof (Chrome / Edge)

Browser-side diagnostics for an already-connected Discord Web call. It does not log in, join or
control calls, read page messages, or record audio.

Chromium decodes Opus as mono unless the SDP `fmtp` line carries `stereo=1`, even when the codec
reports two channels. The page hook adds `stereo=1; sprop-stereo=1` to this tab's Opus `fmtp`
lines so the browser decodes what the sender transmits. It then measures the decoded remote
track's left/right level, side (L-R) level and L/R correlation with Web Audio analysers.
Identical channels (correlation near 1 with no side energy) mean a mono path; only these numbers
leave the page. Start AscendCord with `ASCENDCORD_TEST_TONE=1` set in its environment **before
launching** to send 440 Hz left / 660 Hz right at -12 dBFS. A stereo path reads about -15 dBFS on
each side, correlation near 0 and side -18 dBFS. For the response curve and automatic test call, launch AscendCord with `--test-sweep-channel=<voice-channel-id>`. This explicit argument enables the synthetic sweep and joins only that channel. Without it, AscendCord follows its normal audio and call flow. Turn diagnostic mode off after testing; it replaces live microphone audio with tones.

The page hook watches WebRTC peer connections and sends a bounded summary of inbound audio RTP
statistics to AscendCord over `127.0.0.1:43721`. The local app matches the inbound SSRC against its
current audio sender, so unrelated voice participants are not treated as proof for AscendCord.
The browser reports codec channel count when exposed, decoded-track channel count when exposed,
observed bitrate, packet loss, jitter, concealment, discarded packets and recent decoded energy.

The app displays its own capture format, level, configured Opus/processing settings, measured
RTP-wire send rate, and bounded capture/transport stall counters beside receiver stats. An exact
SSRC match identifies the same RTP source; if Discord changes the SSRC or the browser does not
expose it, the verifier reports no match rather than guessing. Browser stats can confirm codec
channel metadata when exposed, decoded-track activity, receive bitrate, loss, jitter, concealment,
and jitter-buffer delay. They cannot prove the physical speakers are audible or measure every
possible output filter. No media samples or identities are sent to AscendCord.

The popup includes a four-stage injection/forwarding diagnosis, live sender and receiver
summaries, and an unattended receiver-test dashboard. It starts when the exact sender SSRC appears
in browser RTP stats, captures two full 48-band sweep passes, and then stops and prepares the JSON
report automatically. It collects loss, jitter, concealment, discarded packets, bitrate, decoded
levels, and exact-SSRC matches into a bounded timeline. The completed numeric report persists in
extension-local storage across service-worker restarts. It can also be exported as JSON; no audio
samples, Discord messages, tokens, or account names are collected or exported. The full FFT
identifies the received tone; a sine fit to decoded time-domain PCM estimates its peak amplitude.
Mixed-tone transition windows are rejected. The graph plots pipeline gain relative to the known
−12.04 dBFS peak input. One valid sample per band per sweep pass is averaged in linear power, and
error bars show standard deviation across independent passes. The second panel shows gain relative
to the measured median across 20 Hz–20 kHz. To capture a response
curve, launch AscendCord with `--test-sweep-channel=<voice-channel-id>`. The extension captures the matched response curve automatically; keep
Discord joined until the two-pass capture completes. The diagnostic replaces live microphone
content with a 20 Hz–20 kHz equal-level stepped sine sweep at −12.04 dBFS peak. The popup can save the
actual graph canvas as a 2400×1200 PNG; it is rendered from the numeric capture, not generated
imagery.
The resulting curve measures the known 48 kHz test signal from the encoder input, through Opus,
Discord transport and the browser decoder. It does not include the physical microphone, Windows
capture driver, or native-rate resampling, and it cannot prove speaker acoustics. Ordinary voice
or music shows its content spectrum, not a transfer-response curve. The offline Rust graph
separately measures native-rate sample conversion at the source code's resampler boundary.

## Load unpacked

1. Open `chrome://extensions` or `edge://extensions`.
2. Turn on **Developer mode** and choose **Load unpacked**.
3. Select this `stereo-proof` folder. Keep the extension enabled.
4. For an unattended sender join, launch AscendCord with `--test-sweep-channel=<voice-channel-id>`. AscendCord joins that exact channel after
   login and resumes the same target after app restarts. The target must be a voice channel the
   signed-in account can access. The app will not guess a channel or interrupt a different active
   call. Without this argument, AscendCord follows its normal audio and call flow.
5. Keep the Discord browser call joined. The extension automatically starts receiver capture
   when the browser sees the active AscendCord synthetic sweep, stops after two sweep passes,
   and prepares and persists a numeric JSON report. Open the popup to review it, save the measured
   graph as PNG, or export the JSON report.

After editing an unpacked install, use the extension card's **Reload** button and reload the
Discord tab. The manifest version for this diagnostic dashboard is 0.3.3.

The desktop listener is bound only to IPv4 loopback, requires this extension's fixed origin, rejects
large or unknown report fields, and keeps the latest report in memory for five seconds. The health
endpoint accepts only bounded stage labels, counters, and error text. Receiver-test history and
the automatically prepared report stay in extension-local storage until the user resets the test;
no audio is recorded or sent.
