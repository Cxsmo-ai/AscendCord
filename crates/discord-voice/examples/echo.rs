//! Prints the Acheron-shaped desktop voice configuration without opening devices.
use model::voice_settings::VoiceProcessing;

fn main() {
	let settings = VoiceProcessing::default().normalized();
	println!(
		"Audio backend: miniaudio · S16 stereo · 48 kHz · 20 ms periods\n+		Opus: {:?}, {} kbps, complexity {}, {:?}, FEC {}, expected loss {}%\n+		RNNoise suppression {}, RNNoise VAD {}, RMS threshold {}",
		settings.opus.application,
		settings.opus.bitrate / 1_000,
		settings.opus.complexity,
		settings.opus.signal,
		settings.opus.fec,
		settings.opus.packet_loss_percent,
		settings.noise_suppression,
		settings.use_rnnoise_vad,
		settings.vad_threshold_rms,
	);
}
