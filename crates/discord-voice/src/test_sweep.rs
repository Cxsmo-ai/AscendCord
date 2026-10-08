//! Measurement program 3 for the opt-in browser receive test (`--test-sweep-channel`).
//!
//! One pass, repeated: 1.2 s of digital silence; 48 tones from 20 Hz to 20 kHz with left and
//! right identical; every fourth of those tones on the left only, on the right only and in
//! opposite phase; ten steps of rising level up to +3 dBFS, each at its own frequency between
//! two sweep tones; then real-signal content (transient bursts, stereo noise, plucked music,
//! a speech-like voice) and, with `--test-sweep-music=<dir>`, short song clips. Tones last
//! 700 ms (ladder steps 800 ms) with 50 ms fades, at −12.04 dBFS peak except the ladder.
//! `browser-extension/stereo-proof/lab.js` renders the same program from the same formulas and
//! subtracts the content from what arrives.
use std::sync::OnceLock;

const POINTS: usize = 48;
const LOW_HZ: f64 = 20.0;
const HIGH_HZ: f64 = 20_000.0;
const AMPLITUDE: f64 = 0.25;
const STEP_MS: u64 = 700;
const LADDER_STEP_MS: u64 = 800;
const SILENCE_MS: u64 = 1_200;
const FADE_MS: u64 = 50;
const CHANNEL_STRIDE: usize = 4;
const LADDER_DBFS: [f64; 10] = [
	-60.0, -48.0, -36.0, -24.0, -18.0, -12.0, -6.0, -1.0, 0.0, 3.0,
];
const LADDER_FIRST: f64 = 26.5;
pub const PROGRAM_VERSION: u8 = 3;

const CONTENT_FADE_MS: u64 = 20;
const TRANSIENT_COUNT: usize = 8;
const TRANSIENT_FIRST_MS: f64 = 125.0;
const TRANSIENT_SPACING_MS: f64 = 250.0;
const TRANSIENT_BURST_MS: f64 = 2.0;
const TRANSIENT_PEAK: f64 = 0.7;
const NOISE_SCALE: f64 = 0.436;
const MUSIC_HZ: [f64; 6] = [196.0, 246.94, 293.66, 392.0, 587.33, 880.0];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Content {
	Transients,
	Noise,
	Music,
	Speech,
}

const CONTENT: [(Content, u64); 4] = [
	(Content::Transients, 2_000),
	(Content::Noise, 1_500),
	(Content::Music, 2_000),
	(Content::Speech, 2_000),
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
	Silence,
	Mono,
	Left,
	Right,
	Antiphase,
	Content(Content),
	Song(usize),
}

#[derive(Clone, Copy, Debug)]
struct Segment {
	kind: Kind,
	hz: f64,
	peak: f64,
	frames: u64,
}

fn frequency(step: f64) -> f64 {
	LOW_HZ * (HIGH_HZ / LOW_HZ).powf(step / (POINTS - 1) as f64)
}

/// Song clips for the program: 48 kHz interleaved stereo 16-bit, from `--test-sweep-music`.
pub struct Songs {
	pub names: Vec<String>,
	clips: Vec<Vec<i16>>,
	bytes: Vec<u8>,
}

const MAX_SONGS: usize = 16;
const MAX_SONG_FRAMES: usize = 48_000 * 10;

fn music_dir_from_args(
	args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
	let mut args = args.into_iter();
	while let Some(arg) = args.next() {
		let Some(text) = arg.to_str() else {
			continue;
		};
		if let Some(value) = text.strip_prefix("--test-sweep-music=") {
			return Some(value.into());
		}
		if text == "--test-sweep-music" {
			return args.next().map(Into::into);
		}
	}
	None
}

/// Reads `manifest.tsv` (`id<TAB>name` per line) and each `<id>.s16` beside it.
fn load_songs(dir: &std::path::Path) -> Option<Songs> {
	let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).ok()?;
	let mut songs = Songs {
		names: Vec::new(),
		clips: Vec::new(),
		bytes: Vec::new(),
	};
	for line in manifest.lines().take(MAX_SONGS) {
		let Some((id, name)) = line.split_once('\t') else {
			continue;
		};
		if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
			continue;
		}
		let Ok(raw) = std::fs::read(dir.join(format!("{id}.s16"))) else {
			continue;
		};
		let frames = (raw.len() / 4).min(MAX_SONG_FRAMES);
		if frames == 0 {
			continue;
		}
		let raw = &raw[..frames * 4];
		songs.clips.push(
			raw.as_chunks::<2>()
				.0
				.iter()
				.map(|pair| i16::from_le_bytes(*pair))
				.collect(),
		);
		songs.names.push(name.chars().take(64).collect());
		songs.bytes.extend_from_slice(raw);
	}
	(!songs.clips.is_empty()).then_some(songs)
}

/// The song clips this process plays, if `--test-sweep-music` named a usable folder.
pub fn songs() -> Option<&'static Songs> {
	static SONGS: OnceLock<Option<Songs>> = OnceLock::new();
	SONGS
		.get_or_init(|| load_songs(&music_dir_from_args(std::env::args_os().skip(1))?))
		.as_ref()
}

impl Songs {
	/// Name and length of each clip, in program order.
	pub fn manifest(&self) -> serde_json::Value {
		serde_json::Value::Array(
			self.names
				.iter()
				.zip(&self.clips)
				.map(|(name, clip)| serde_json::json!({"name": name, "frames": clip.len() / 2}))
				.collect(),
		)
	}

	/// Every clip's samples back to back, as read: 16-bit little-endian, interleaved.
	pub fn pcm(&self) -> &[u8] {
		&self.bytes
	}
}

fn program(rate: u32, songs: Option<&Songs>) -> Vec<Segment> {
	let frames = |ms: u64| u64::from(rate) * ms / 1_000;
	let tone = |kind, hz, peak, ms| Segment {
		kind,
		hz,
		peak,
		frames: frames(ms),
	};
	let mut segments = vec![tone(Kind::Silence, 0.0, 0.0, SILENCE_MS)];
	for step in 0..POINTS {
		segments.push(tone(Kind::Mono, frequency(step as f64), AMPLITUDE, STEP_MS));
	}
	for kind in [Kind::Left, Kind::Right, Kind::Antiphase] {
		for step in (0..POINTS).step_by(CHANNEL_STRIDE) {
			segments.push(tone(kind, frequency(step as f64), AMPLITUDE, STEP_MS));
		}
	}
	for (index, dbfs) in LADDER_DBFS.into_iter().enumerate() {
		segments.push(tone(
			Kind::Mono,
			frequency(LADDER_FIRST + index as f64),
			10f64.powf(dbfs / 20.0),
			LADDER_STEP_MS,
		));
	}
	for (content, ms) in CONTENT {
		segments.push(tone(Kind::Content(content), 0.0, 0.0, ms));
	}
	// Song clips are 48 kHz; at another rate they are left out rather than resampled.
	if let Some(songs) = songs.filter(|_| rate == 48_000) {
		for (index, clip) in songs.clips.iter().enumerate() {
			segments.push(Segment {
				kind: Kind::Song(index),
				hz: 0.0,
				peak: 0.0,
				frames: (clip.len() / 2) as u64,
			});
		}
	}
	segments
}

fn hash32(value: u32) -> u32 {
	let mut x = value;
	x ^= x >> 16;
	x = x.wrapping_mul(0x7feb_352d);
	x ^= x >> 15;
	x = x.wrapping_mul(0x846c_a68b);
	x ^= x >> 16;
	x
}

/// Uniform noise in [-0.5, 0.5) for stream `seed` at sample `n`, as `lab.js` `noiseAt`.
fn noise_at(seed: u32, n: u32) -> f64 {
	f64::from(hash32(seed.wrapping_mul(0x9e37_79b9).wrapping_add(n))) / 4_294_967_296.0 - 0.5
}

fn transient_onsets(rate: u32) -> [u64; TRANSIENT_COUNT] {
	std::array::from_fn(|k| {
		(f64::from(rate) * (TRANSIENT_FIRST_MS + k as f64 * TRANSIENT_SPACING_MS) / 1_000.0).round()
			as u64
	})
}

fn formant(hz: f64) -> f64 {
	let mut weight = 0.05;
	for center in [500.0, 1_500.0, 2_500.0] {
		weight += 1.0 / (1.0 + ((hz - center) / 150.0).powi(2));
	}
	weight
}

/// One stereo sample of a content section before its fade; the same arithmetic as `lab.js`.
fn content_sample(
	content: Content,
	i: u64,
	rate: u32,
	onsets: &[u64; TRANSIENT_COUNT],
) -> [f64; 2] {
	use std::f64::consts::PI;
	let t = i as f64 / f64::from(rate);
	match content {
		Content::Transients => {
			let burst = (f64::from(rate) * TRANSIENT_BURST_MS / 1_000.0).round() as u64;
			for (k, &onset) in onsets.iter().enumerate() {
				if i >= onset && i - onset < burst {
					let local = i - onset;
					let window = 0.5 - 0.5 * (2.0 * PI * (local as f64 + 0.5) / burst as f64).cos();
					let value =
						window * noise_at(1_000 + k as u32, local as u32) * 2.0 * TRANSIENT_PEAK;
					return [value, value];
				}
			}
			[0.0, 0.0]
		}
		Content::Noise => [
			noise_at(2_001, i as u32) * NOISE_SCALE,
			noise_at(2_002, i as u32) * NOISE_SCALE,
		],
		Content::Music => {
			let since = t % 0.5;
			let envelope = (since / 0.005).min(1.0) * (-since / 0.25).exp();
			let wobble = 0.004 / (2.0 * PI * 5.5) * (1.0 - (2.0 * PI * 5.5 * t).cos());
			let (mut left, mut right) = (0.0, 0.0);
			for (j, hz) in MUSIC_HZ.into_iter().enumerate() {
				let value = 0.09 * envelope * (2.0 * PI * hz * (t + wobble)).sin();
				let angle = (j as f64 + 0.5) / MUSIC_HZ.len() as f64 * PI / 2.0;
				left += value * angle.cos();
				right += value * angle.sin();
			}
			[left, right]
		}
		Content::Speech => {
			let phase = 2.0 * PI * (145.0 * t - 35.0 / PI * (PI * t).sin());
			let f0 = 145.0 - 35.0 * (PI * t).cos();
			let twice = 2.0 * phase.cos();
			let (mut previous, mut sine, mut value) = (0.0, phase.sin(), 0.0);
			let mut k = 1u32;
			while k <= 40 && f64::from(k) * f0 < 7_000.0 {
				value += formant(f64::from(k) * f0) * f64::from(k).powf(-0.7) * sine;
				let next = twice * sine - previous;
				previous = sine;
				sine = next;
				k += 1;
			}
			let syllable = (0.5 - 0.5 * (2.0 * PI * 4.0 * t).cos()).powi(2);
			[0.12 * syllable * value, 0.12 * syllable * value]
		}
	}
}

pub(crate) struct Sweep {
	rate: u32,
	segments: Vec<Segment>,
	songs: Option<&'static Songs>,
	onsets: [u64; TRANSIENT_COUNT],
	segment: usize,
	within: u64,
	phase: f64,
}

impl Sweep {
	pub(crate) fn new(rate: u32) -> Self {
		Self::with_songs(rate, songs())
	}

	fn with_songs(rate: u32, songs: Option<&'static Songs>) -> Self {
		Self {
			rate,
			segments: program(rate, songs),
			songs,
			onsets: transient_onsets(rate),
			segment: 0,
			within: 0,
			phase: 0.0,
		}
	}

	pub(crate) fn next_frame(&mut self) -> [f32; 2] {
		let segment = self.segments[self.segment];
		let within = self.within;
		self.within += 1;
		if self.within >= segment.frames {
			self.within = 0;
			self.segment = (self.segment + 1) % self.segments.len();
		}
		let content_ramp = || {
			let fade = (u64::from(self.rate) * CONTENT_FADE_MS / 1_000) as f64;
			(within.min(segment.frames - within) as f64 / fade).min(1.0)
		};
		match segment.kind {
			Kind::Content(content) => {
				let ramp = content_ramp();
				let [left, right] = content_sample(content, within, self.rate, &self.onsets);
				return [(left * ramp) as f32, (right * ramp) as f32];
			}
			Kind::Song(index) => {
				let ramp = content_ramp();
				let clip = &self.songs.expect("songs in the program").clips[index];
				let sample = |channel: usize| {
					f64::from(f32::from(clip[within as usize * 2 + channel]) / 32_768.0)
				};
				return [(sample(0) * ramp) as f32, (sample(1) * ramp) as f32];
			}
			_ => {}
		}
		let fade_frames = u64::from(self.rate) * FADE_MS / 1_000;
		let fade = (within.min(segment.frames - within) as f64 / fade_frames as f64).min(1.0);
		let value = (self.phase.sin() * segment.peak * fade) as f32;
		self.phase += std::f64::consts::TAU * segment.hz / f64::from(self.rate);
		if self.phase >= std::f64::consts::TAU {
			self.phase %= std::f64::consts::TAU;
		}
		if self.within == 0 {
			self.phase = 0.0;
		}
		match segment.kind {
			Kind::Silence => [0.0, 0.0],
			Kind::Left => [value, 0.0],
			Kind::Right => [0.0, value],
			Kind::Antiphase => [value, -value],
			_ => [value, value],
		}
	}
}

/// Frames in one pass without songs, at `rate`.
#[cfg(test)]
pub(crate) fn pass_frames(rate: u32) -> usize {
	program(rate, None)
		.iter()
		.map(|segment| segment.frames as usize)
		.sum()
}

#[cfg(test)]
mod tests {
	use super::*;

	const RATE: u32 = 48_000;

	/// Frames of one segment from a sweep positioned at its start.
	fn segment_frames(sweep: &mut Sweep, segment: Segment) -> Vec<[f32; 2]> {
		(0..segment.frames).map(|_| sweep.next_frame()).collect()
	}

	#[test]
	fn program_matches_the_extension_grid_and_order() {
		let segments = program(RATE, None);
		assert_eq!(
			segments.len(),
			1 + POINTS + 3 * POINTS / CHANNEL_STRIDE + LADDER_DBFS.len() + CONTENT.len()
		);
		assert_eq!(segments[0].kind, Kind::Silence);
		assert_eq!(segments[1].hz, LOW_HZ);
		assert!((segments[POINTS].hz - HIGH_HZ).abs() < 1e-6);
		for window in segments[1..=POINTS].windows(2) {
			assert!(window[1].hz > window[0].hz);
		}
		let ladder =
			&segments[segments.len() - CONTENT.len() - LADDER_DBFS.len()..][..LADDER_DBFS.len()];
		// The ladder sits between sweep tones 26..36, never on one.
		for (index, segment) in ladder.iter().enumerate() {
			let low = frequency(26.0 + index as f64);
			let high = frequency(27.0 + index as f64);
			assert!(segment.hz > low * 1.03 && segment.hz < high / 1.03);
			assert!((20.0 * segment.peak.log10() - LADDER_DBFS[index]).abs() < 1e-9);
		}
		let pass_ms: u64 = segments
			.iter()
			.map(|segment| segment.frames * 1_000 / u64::from(RATE))
			.sum();
		assert_eq!(
			pass_ms,
			SILENCE_MS + (POINTS as u64 + 36) * STEP_MS + 10 * LADDER_STEP_MS + 7_500
		);
	}

	#[test]
	fn every_section_drives_the_channels_it_names() {
		let mut sweep = Sweep::with_songs(RATE, None);
		for segment in program(RATE, None) {
			let frames = segment_frames(&mut sweep, segment);
			for [left, right] in &frames {
				match segment.kind {
					Kind::Silence => assert_eq!((*left, *right), (0.0, 0.0)),
					Kind::Mono => assert_eq!(left.to_bits(), right.to_bits()),
					Kind::Left => assert_eq!(*right, 0.0),
					Kind::Right => assert_eq!(*left, 0.0),
					Kind::Antiphase => assert_eq!(*left, -*right),
					Kind::Content(_) | Kind::Song(_) => {}
				}
				if !matches!(segment.kind, Kind::Mono) {
					assert!(left.abs() <= 0.9 && right.abs() <= 0.9, "{segment:?}");
				}
			}
		}
		// The program repeats from silence.
		assert_eq!(sweep.next_frame(), [0.0, 0.0]);
	}

	#[test]
	fn tones_hold_their_level_between_fades() {
		let mut sweep = Sweep::with_songs(RATE, None);
		let fade = u64::from(RATE) * FADE_MS / 1_000;
		for segment in program(RATE, None) {
			let frames = segment_frames(&mut sweep, segment);
			if !matches!(
				segment.kind,
				Kind::Mono | Kind::Left | Kind::Right | Kind::Antiphase
			) {
				continue;
			}
			let level = |[left, right]: [f32; 2]| left.abs().max(right.abs());
			assert!(level(frames[0]) < 0.001);
			assert!(level(*frames.last().unwrap()) < 0.001);
			// Two cycles of the slowest tone fit in the steady part, so its peak is reached.
			let steady = &frames[fade as usize..(segment.frames - fade) as usize];
			let peak = steady.iter().copied().map(level).fold(0.0f32, f32::max);
			assert!(
				(f64::from(peak) - segment.peak).abs() < segment.peak * 0.01,
				"{segment:?}"
			);
		}
	}

	#[test]
	fn content_is_the_closed_form_the_extension_renders() {
		// Exact float32 values of lab.js renderContent at 48 kHz, away from the fades.
		let onsets = transient_onsets(RATE);
		assert_eq!(onsets[0], 6_000);
		assert_eq!(onsets[7], 90_000);
		for (content, local, left, right) in [
			(Content::Transients, 6_040, 0x3e02_7faa, 0x3e02_7faa),
			(Content::Noise, 5_000, 0xbc82_6a20, 0x3e18_36e4),
			(Content::Music, 50_000, 0x3df0_94b8, 0xbd80_7000),
			(Content::Speech, 40_000, 0xbc34_3279, 0xbc34_3279),
			(Content::Speech, 90_001, 0xbc58_3e3f, 0xbc58_3e3f),
		] {
			let [l, r] = content_sample(content, local, RATE, &onsets);
			assert_eq!(
				((l as f32).to_bits(), (r as f32).to_bits()),
				(left, right),
				"{content:?} {local}"
			);
		}
	}

	#[test]
	fn songs_follow_the_content_and_are_read_from_a_manifest() {
		let dir = std::env::temp_dir().join(format!("ascendcord-songs-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(
			dir.join("manifest.tsv"),
			"01\tFirst\n../x\tEscape\n02\tSecond\n",
		)
		.unwrap();
		let clip: Vec<u8> = (0..960i16)
			.flat_map(|n| [n, -n])
			.flat_map(i16::to_le_bytes)
			.collect();
		std::fs::write(dir.join("01.s16"), &clip).unwrap();
		std::fs::write(dir.join("02.s16"), &clip[..400]).unwrap();
		let songs = Box::leak(Box::new(load_songs(&dir).unwrap()));
		std::fs::remove_dir_all(&dir).unwrap();
		assert_eq!(songs.names, ["First", "Second"]);
		assert_eq!(songs.manifest()[1]["frames"], 100);
		assert_eq!(songs.pcm().len(), clip.len() + 400);
		let segments = program(RATE, Some(songs));
		assert_eq!(segments[segments.len() - 2].kind, Kind::Song(0));
		// A song sample is its 16-bit value over 32768, faded in over 20 ms.
		let mut sweep = Sweep::with_songs(RATE, Some(songs));
		let before: usize = segments[..segments.len() - 2]
			.iter()
			.map(|segment| segment.frames as usize)
			.sum();
		for _ in 0..before + 10 {
			sweep.next_frame();
		}
		let expected = (f64::from(10.0f32 / 32_768.0) * (10.0 / 960.0)) as f32;
		assert_eq!(sweep.next_frame()[0].to_bits(), expected.to_bits());
		assert_eq!(
			music_dir_from_args(["--x".into(), "--test-sweep-music=C:/m".into()]),
			Some("C:/m".into())
		);
	}
}
