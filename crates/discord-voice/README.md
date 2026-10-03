# Discord voice adapter

Native media for an explicitly joined one-to-one or group voice channel. The desktop owns call intent and audio device lifetime; this crate owns voice signaling, RTP media, Opus, stereo mixing and the ephemeral DAVE group. It does not record calls, retain DAVE keys or use a relay.

## Audio path

The Windows desktop now uses miniaudio with Acheron's device contract: S16 PCM, two channels, 48 kHz and 960 frames per period. Miniaudio performs device-format conversion. Capture and playback callbacks only move samples through bounded lock-free rings; capture DSP, Opus and speaker mixing run on worker/transport threads.

Optional RNNoise operates independently on left and right. RNNoise probability VAD or the raw RMS gate controls transmission with a 500 ms hold. The mic path has no echo canceller, automatic gain controller or WebRTC audio processing. Stereo receive decodes to two channels, applies per-user level, mixes L-to-L and R-to-R, then plays to the selected output device.

The native settings match Acheron's visible controls: VoIP/Audio application, 8–510 kbps bitrate, 0–10 complexity, Auto/Voice/Music signal, FEC, expected packet loss, RNNoise suppression, RNNoise VAD and 0–2,000 RMS threshold. Input/output gain are independently adjustable from 0–200%. The encoder remains stereo, fullband and DTX-off. These settings are persisted locally.

## Discord transport

Voice Gateway v8, RTP, Opus and DAVE v1 are used for current voice calls. Media is not sent before the authenticated DAVE group is ready; transport AEAD remains active outside DAVE frame encryption. Voice credentials are sent only to validated Discord media endpoints. Account/session tokens are not included in diagnostics. Device callbacks and local queues never retain recordings.

Per-speaker jitter buffers adapt from three packets toward a 2–8 packet range, with Opus packet-loss concealment. Voice, camera and screen-share retain separate capture/encode paths. Camera and screen video continue to share the call's authenticated voice transport without routing through the microphone pipeline.

The Opus bitrate/complexity/signal/FEC controls affect the next voice frame. Changing the Opus application recreates the encoder; other options update in place. DAVE and RTP security transitions continue to pause media and clear stale capture frames.

The small offline example prints the current default configuration without opening devices. A live Discord compatibility check still requires a private call with a second viewer; a successful compile alone does not verify device-driver behavior, latency or remote playback quality.