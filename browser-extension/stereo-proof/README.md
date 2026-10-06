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
each side, correlation near 0 and side -18 dBFS. For the response curve, use
`ASCENDCORD_TEST_SWEEP=1` instead. Turn diagnostic mode off after testing; it replaces live
microphone audio with tones.

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
summaries, and a receiver-test dashboard. It auto-arms a capture when the synthetic sweep is
enabled, starts when the exact sender SSRC appears in browser RTP stats, and captures one full
48-band sweep pass (33.6 seconds plus a short margin). It then stops and prepares the JSON report
automatically. The controls collect loss, jitter, concealment, discarded packets, bitrate, decoded levels,
and exact-SSRC matches into a bounded timeline. The timeline can be exported as a local JSON file.
It contains numeric WebRTC diagnostics only; no audio samples, Discord messages, tokens, or account
names are collected or exported. To compare
the response curve, the popup also graphs a live, logarithmic 20 Hz–20 kHz spectrum from the exact
matched receiver. The graph updates while the popup is open and holds only 48 numeric spectrum
bins in extension memory. To capture a response curve, set
`ASCENDCORD_TEST_SWEEP=1` in the AscendCord process environment before launching it, and join the
controlled test call. The extension captures the matched response curve automatically; keep
Discord joined until the capture completes. The diagnostic
replaces live microphone content with a 20 Hz–20 kHz equal-level stepped sine sweep at −12 dBFS.
The resulting curve measures the known 48 kHz test signal from the encoder input, through Opus,
Discord transport and the browser decoder. It does not include the physical microphone, Windows
capture driver, or native-rate resampling, and it cannot prove speaker acoustics. Ordinary voice
or music shows its content spectrum, not a transfer-response curve. The offline Rust graph
separately measures native-rate sample conversion at the source code's resampler boundary.

## Load unpacked

1. Open `chrome://extensions` or `edge://extensions`.
2. Turn on **Developer mode** and choose **Load unpacked**.
3. Select this `stereo-proof` folder. Keep the extension enabled.
4. Keep AscendCord open and join the same voice call from the browser alt account.
5. Keep the Discord browser call joined. The extension automatically starts receiver capture
   when the browser sees the active AscendCord synthetic sweep, stops after one full sweep pass,
   and prepares a numeric JSON report in extension memory. Open the popup to review it; downloading
   the prepared JSON file is optional.

After editing an unpacked install, use the extension card's **Reload** button and reload the
Discord tab. The manifest version for this diagnostic dashboard is 0.3.1.

The desktop listener is bound only to IPv4 loopback, requires this extension's fixed origin, rejects
large or unknown report fields, and keeps the latest report in memory for five seconds. The health
endpoint accepts only bounded stage labels, counters, and error text. Receiver-test history and
the automatically prepared report stay inside extension memory until the user resets the test;
no audio is recorded or sent.
