# Tesktop Stereo Proof (Chrome / Edge)

Browser-side diagnostics for an already-connected Discord Web call. It does not log in, join or
control calls, read page messages, or record audio.

Chromium decodes Opus as mono unless the SDP `fmtp` line carries `stereo=1`, even when the codec
reports two channels. The page hook adds `stereo=1; sprop-stereo=1` to this tab's Opus `fmtp`
lines so the browser decodes what the sender transmits. It then measures the decoded remote
track's left/right level, side (L-R) level and L/R correlation with Web Audio analysers.
Identical channels (correlation near 1 with no side energy) mean a mono path; only these numbers
leave the page. Start Tesktop with `TESKTOP_TEST_TONE=1` to send 440 Hz left / 660 Hz right at
-12 dBFS: a stereo path reads about -15 dBFS on each side, correlation near 0 and side -18 dBFS.

The page hook watches WebRTC peer connections and sends a bounded summary of inbound audio RTP
statistics to Tesktop over `127.0.0.1:43721`. The local app matches the inbound SSRC against its
current audio sender, so unrelated voice participants are not treated as proof for Tesktop.
The browser reports codec channel count when exposed, decoded-track channel count when exposed,
observed bitrate, packet loss, jitter, concealment, discarded packets and recent decoded energy.

The app displays its own capture format, level, configured Opus/processing settings, measured
RTP-wire send rate, and bounded capture/transport stall counters beside receiver stats. An exact
SSRC match identifies the same RTP source; if Discord changes the SSRC or the browser does not
expose it, the verifier reports no match rather than guessing. Browser stats can confirm codec
channel metadata when exposed, decoded-track activity, receive bitrate, loss, jitter, concealment,
and jitter-buffer delay. They cannot prove the physical speakers are audible or measure every
possible output filter. No media samples or identities are sent to Tesktop.

## Load unpacked

1. Open `chrome://extensions` or `edge://extensions`.
2. Turn on **Developer mode** and choose **Load unpacked**.
3. Select this `stereo-proof` folder. Keep the extension enabled.
4. Keep Tesktop open and join the same voice call from the browser alt account.
5. Open the extension popup or Tesktop's voice settings to see matched sender/receiver stats.

The desktop listener is bound only to IPv4 loopback, requires this extension's fixed origin, rejects
large or unknown report fields, and keeps the latest report in memory for five seconds. Only numeric
transport statistics and the ephemeral audio SSRC cross the local connection.
