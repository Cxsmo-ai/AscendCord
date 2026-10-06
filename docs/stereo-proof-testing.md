# Stereo Proof audio testing

Stereo Proof measures numeric WebRTC receive statistics and the decoded sweep response in Edge or
Chrome. It does not capture or export audio. The test sweep replaces the microphone signal while
it is active, so use it in a test call and stop the app when the capture finishes.

## Install the extension

1. Download `ascendcord-vX.Y.Z-Stereo-Proof-Extension.zip` from the
   [latest release](https://github.com/Cxsmo-ai/AscendCord/releases/latest) and extract it.
2. Open `edge://extensions` or `chrome://extensions` and turn on **Developer mode**.
3. Choose **Load unpacked** and select the extracted extension folder containing `manifest.json`.
4. Open Discord Web and join the test voice channel with the receiving account. Keep the tab open.

## Run a matched sweep

1. Copy the voice-channel ID from Discord with Developer Mode enabled. The test account must have
   permission to join and speak in this channel.
2. Launch AscendCord with the channel ID as one explicit argument. In PowerShell, for an installed
   copy, run:

   ```powershell
   & "$env:LOCALAPPDATA\Programs\AscendCord\ascendcord.exe" --test-sweep-channel=123456789012345678
   ```

   Replace the numeric value with the test voice-channel ID. For a portable build, run the same
   argument after the path to `ascendcord.exe` in the extracted release folder.
3. AscendCord logs in, joins that exact channel, and sends a 20 Hz–20 kHz stepped sine sweep. The
   physical microphone is bypassed only for this process launch. The extension starts the matched
   receiver capture automatically when it sees AscendCord's sender SSRC, and finishes after two
   sweep passes.
4. Open **AscendCord Stereo Proof** from the browser toolbar to review the numeric report. Use
   **Download JSON** for the metrics and **Save high-resolution graph (PNG)** for the measured
   response graph.
5. After two matched sweep passes, the receiver report is saved automatically and AscendCord leaves
   the test call. Open the popup at any time to review or export the saved JSON and measured graph.

The extension cannot join the browser account to voice. Join Discord Web with the receiving account
before starting the AscendCord sender. The sender joins its configured test channel automatically.

Without `--test-sweep-channel=<ID>`, AscendCord does not auto-join for this test and does not replace
microphone audio with the sweep. It follows its normal call and audio behavior. Do not set the legacy
`ASCENDCORD_TEST_TONE=1` variable during a normal audio session; that separate stereo diagnostic
also substitutes generated test audio.

The exported report contains bounded numeric WebRTC metrics and spectrum measurements only. It does
not contain audio, message content, account names, credentials, or browser cookies. The graph
describes the measured encoder-to-browser path for this run; it does not measure speakers or
physical microphone frequency response.

For protocol and implementation details, see the extension's
[README](../browser-extension/stereo-proof/README.md). Please include the release version, report
JSON, and graph with audio-test bug reports. Never attach account tokens, cookies, or private audio.
