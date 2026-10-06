//! Loopback-only, diagnostics-only bridge between Tesktop and the browser verifier.
//! It carries bounded RTP statistics and sender settings; never credentials or media.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
	io::{BufRead, BufReader, Read, Write},
	net::{TcpListener, TcpStream},
	sync::{
		Arc, Mutex,
		atomic::{AtomicBool, Ordering},
	},
	thread::{self, JoinHandle},
	time::{Duration, Instant},
};

const PORT: u16 = 43_721;
const EXTENSION_ORIGIN: &str = "chrome-extension://jbchdifpgimmmmlnimidbfockpbigfni";
const MAX_HEADER_BYTES: usize = 4 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024;
const MAX_STREAMS: usize = 16;
const REPORT_MAX_AGE: Duration = Duration::from_secs(5);
const DIAGNOSTICS_MAX_AGE: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiverReport {
	pub protocol: u8,
	pub sampled_at_ms: u64,
	pub peer_connections: u8,
	pub streams: Vec<InboundAudio>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtensionDiagnostics {
	pub protocol: u8,
	pub sampled_at_ms: u64,
	pub content_bridge: bool,
	pub observer_state: String,
	pub observer_api: bool,
	pub observer_error: String,
	pub peer_connections: u8,
	pub inbound_streams: u8,
	pub report_count: u32,
	pub forward_error: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sweep_test: Option<SweepTestDiagnostics>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub sweep_curve: Option<SweepCurveDiagnostics>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SweepTestDiagnostics {
	pub running: bool,
	pub automatic: bool,
	pub capture_active: bool,
	pub export_ready: bool,
	pub sender_ssrc: Option<u32>,
	pub sample_count: u16,
	pub matched_samples: u16,
	pub average_loss_percent: Option<f32>,
	pub peak_loss_percent: Option<f32>,
	pub average_jitter_ms: Option<f32>,
	pub peak_jitter_ms: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SweepCurveDiagnostics {
	pub ssrc: u32,
	pub spectrum_dbfs: Vec<f32>,
}

impl ExtensionDiagnostics {
	fn validate(&self) -> bool {
		self.protocol == 1
			&& self.observer_state.len() <= 48
			&& self
				.observer_state
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
			&& self.observer_error.len() <= 180
			&& self.forward_error.len() <= 180
			&& self.peer_connections <= 32
			&& self.inbound_streams <= 16
			&& self
				.sweep_test
				.as_ref()
				.is_none_or(SweepTestDiagnostics::validate)
			&& self
				.sweep_curve
				.as_ref()
				.is_none_or(SweepCurveDiagnostics::validate)
			&& self
				.sweep_test
				.as_ref()
				.zip(self.sweep_curve.as_ref())
				.is_none_or(|(test, curve)| test.sender_ssrc.is_none_or(|ssrc| ssrc == curve.ssrc))
	}
}

impl SweepTestDiagnostics {
	fn validate(&self) -> bool {
		self.sample_count <= 1800
			&& self.matched_samples <= self.sample_count
			&& [self.average_loss_percent, self.peak_loss_percent]
				.into_iter()
				.flatten()
				.all(|value| value.is_finite() && (0.0..=100.0).contains(&value))
			&& [self.average_jitter_ms, self.peak_jitter_ms]
				.into_iter()
				.flatten()
				.all(|value| value.is_finite() && (0.0..=60_000.0).contains(&value))
	}
}

impl SweepCurveDiagnostics {
	fn validate(&self) -> bool {
		self.spectrum_dbfs.len() == 48
			&& self
				.spectrum_dbfs
				.iter()
				.all(|value| value.is_finite() && (-120.0..=12.0).contains(value))
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboundAudio {
	pub ssrc: Option<u32>,
	pub codec: String,
	pub channels: Option<u8>,
	pub track_channels: Option<u8>,
	pub sample_rate_hz: Option<u32>,
	pub bitrate_bps: u32,
	pub packets_received: u64,
	pub packets_lost: i64,
	pub loss_percent: f32,
	pub jitter_ms: f32,
	pub concealed_samples: u64,
	pub concealment_events: u64,
	pub discarded_packets: u64,
	pub audio_level: f32,
	pub active: bool,
	pub concealed_samples_per_second: f32,
	pub concealment_events_delta: u64,
	pub discarded_packets_delta: u64,
	pub jitter_buffer_delay_ms: f32,
	/// Whether the negotiated receive fmtp asks Opus for stereo decoding.
	#[serde(default)]
	pub sdp_fmtp_stereo: Option<bool>,
	/// Decoded-track channel levels and correlation, measured in the browser tab.
	#[serde(default)]
	pub left_dbfs: Option<f32>,
	#[serde(default)]
	pub right_dbfs: Option<f32>,
	#[serde(default)]
	pub side_dbfs: Option<f32>,
	#[serde(default)]
	pub lr_correlation: Option<f32>,
}

/// What the decoded channels show. Identical channels mean a mono path somewhere
/// between capture and the browser's decoder; silence proves nothing either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StereoVerdict {
	Unmeasured,
	Silent,
	Mono,
	Stereo,
}

impl InboundAudio {
	pub fn stereo_verdict(&self) -> StereoVerdict {
		let (Some(left), Some(right)) = (self.left_dbfs, self.right_dbfs) else {
			return StereoVerdict::Unmeasured;
		};
		if left.max(right) < -70.0 {
			return StereoVerdict::Silent;
		}
		let side = self.side_dbfs.unwrap_or(-120.0);
		let correlation = self.lr_correlation.unwrap_or(1.0);
		// Side energy 40 dB under the louder channel is a numerically mono signal.
		if correlation > 0.995 && side < left.max(right) - 40.0 {
			StereoVerdict::Mono
		} else {
			StereoVerdict::Stereo
		}
	}
}

impl ReceiverReport {
	fn validate(&self) -> bool {
		self.protocol == 1
			&& self.peer_connections <= 32
			&& self.streams.len() <= MAX_STREAMS
			&& self.streams.iter().all(InboundAudio::validate)
	}
}

impl InboundAudio {
	fn validate(&self) -> bool {
		!self.codec.is_empty()
			&& self.codec.len() <= 32
			&& self
				.codec
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || b"/-._".contains(&byte))
			&& self
				.channels
				.is_none_or(|channels| (1..=8).contains(&channels))
			&& self
				.track_channels
				.is_none_or(|channels| (1..=8).contains(&channels))
			&& self
				.sample_rate_hz
				.is_none_or(|rate| (8_000..=384_000).contains(&rate))
			&& self.bitrate_bps <= 10_000_000
			&& self.packets_received < (1 << 60)
			&& self.packets_lost.unsigned_abs() < (1 << 60)
			&& self.loss_percent.is_finite()
			&& (0.0..=100.0).contains(&self.loss_percent)
			&& self.jitter_ms.is_finite()
			&& (0.0..=60_000.0).contains(&self.jitter_ms)
			&& self.concealed_samples < (1 << 60)
			&& self.concealment_events < (1 << 60)
			&& self.discarded_packets < (1 << 60)
			&& self.audio_level.is_finite()
			&& (0.0..=1.0).contains(&self.audio_level)
			&& self.concealed_samples_per_second.is_finite()
			&& (0.0..=384_000.0).contains(&self.concealed_samples_per_second)
			&& self.concealment_events_delta < (1 << 60)
			&& self.discarded_packets_delta < (1 << 60)
			&& self.jitter_buffer_delay_ms.is_finite()
			&& (0.0..=60_000.0).contains(&self.jitter_buffer_delay_ms)
			&& [self.left_dbfs, self.right_dbfs, self.side_dbfs]
				.into_iter()
				.flatten()
				.all(|db| db.is_finite() && (-120.0..=12.0).contains(&db))
			&& self
				.lr_correlation
				.is_none_or(|value| value.is_finite() && (-1.0..=1.0).contains(&value))
	}
}

#[derive(Default)]
struct Shared {
	sender: Value,
	receiver: Option<ReceiverReport>,
	receiver_at: Option<Instant>,
	extension_diagnostics: Option<ExtensionDiagnostics>,
	extension_diagnostics_at: Option<Instant>,
}

pub(crate) struct Interconnect {
	shared: Arc<Mutex<Shared>>,
	stop: Arc<AtomicBool>,
	worker: Option<JoinHandle<()>>,
	listening: bool,
}

impl Default for Interconnect {
	fn default() -> Self {
		let shared = Arc::new(Mutex::new(Shared::default()));
		let stop = Arc::new(AtomicBool::new(false));
		let listener = TcpListener::bind(("127.0.0.1", PORT)).ok();
		let worker = listener.and_then(|listener| {
			listener.set_nonblocking(true).ok()?;
			let shared = shared.clone();
			let stop = stop.clone();
			thread::Builder::new()
				.name("voice-proof-loopback".into())
				.spawn(move || {
					while !stop.load(Ordering::Acquire) {
						match listener.accept() {
							Ok((stream, _)) => serve(stream, &shared),
							Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
								thread::sleep(Duration::from_millis(20));
							}
							Err(_) => thread::sleep(Duration::from_millis(50)),
						}
					}
				})
				.ok()
		});
		let listening = worker.is_some();
		Self {
			shared,
			stop,
			worker,
			listening,
		}
	}
}

impl Interconnect {
	pub fn set_sender(&self, sender: Value) {
		if let Ok(mut shared) = self.shared.lock() {
			shared.sender = sender;
		}
	}

	pub fn snapshot(&self) -> Value {
		let Ok(shared) = self.shared.lock() else {
			return json!({"enabled": false});
		};
		let fresh = shared
			.receiver_at
			.is_some_and(|at| at.elapsed() <= REPORT_MAX_AGE);
		let diagnostics_fresh = shared
			.extension_diagnostics_at
			.is_some_and(|at| at.elapsed() <= DIAGNOSTICS_MAX_AGE);
		json!({
			"enabled": self.listening,
			"port": if self.listening { Some(PORT) } else { None::<u16> },
			"sender": shared.sender,
			"receiver_connected": fresh,
			"receiver": if fresh { shared.receiver.clone() } else { None },
			"extension_diagnostics_connected": diagnostics_fresh,
			"extension_diagnostics": if diagnostics_fresh { shared.extension_diagnostics.clone() } else { None },
		})
	}

	pub fn summary(&self, sender_ssrc: Option<u32>) -> String {
		if !self.listening {
			return format!("Browser verifier bridge unavailable · local port {PORT} is busy");
		}
		let Ok(shared) = self.shared.lock() else {
			return "Browser verifier status unavailable".into();
		};
		let sender = &shared.sender;
		let send = if sender.is_object() {
			format!(
				"Tesktop send · {} kb/s wire · {} pps · callback drops in {}/{} out {} · loop stalls {}",
				sender["wire_bitrate_bps"].as_u64().unwrap_or(0) / 1000,
				sender["wire_packets_per_second"].as_u64().unwrap_or(0),
				sender["capture_ring_drops"].as_u64().unwrap_or(0),
				sender["capture_worker_drops"].as_u64().unwrap_or(0),
				sender["playback_ring_drops"].as_u64().unwrap_or(0),
				sender["transport_loop_stalls_per_second"]
					.as_u64()
					.unwrap_or(0),
			)
		} else {
			"Tesktop sender unavailable".into()
		};
		let Some(at) = shared.receiver_at else {
			return format!("{send} · waiting for Chrome/Edge receiver stats");
		};
		if at.elapsed() > REPORT_MAX_AGE {
			return format!("{send} · browser extension or receive stats idle");
		}
		let Some(sender_ssrc) = sender_ssrc else {
			return format!("{send} · browser connected; waiting for Tesktop sender SSRC");
		};
		let Some(stream) = shared.receiver.as_ref().and_then(|report| {
			report
				.streams
				.iter()
				.find(|stream| stream.ssrc == Some(sender_ssrc))
		}) else {
			return format!("{send} · browser has inbound audio, but no exact SSRC match yet");
		};
		let codec_channels = stream.channels.map_or_else(
			|| "channel count unavailable".into(),
			|count| format!("{count}ch codec"),
		);
		let track_channels = stream.track_channels.map_or_else(
			|| "track channels unavailable".into(),
			|count| format!("{count}ch decoded track"),
		);
		let loss = stream.loss_percent;
		let jitter = stream.jitter_ms;
		let rate = stream.bitrate_bps / 1000;
		let energy = if stream.active {
			"decoded audio active"
		} else {
			"no recent decoded audio energy"
		};
		let stereo = match stream.stereo_verdict() {
			StereoVerdict::Unmeasured => "L/R not measured yet".to_owned(),
			StereoVerdict::Silent => "L/R silent".to_owned(),
			verdict => format!(
				"{} · L {:.0} / R {:.0} dBFS · side {:.0} dBFS · corr {:.3}",
				if verdict == StereoVerdict::Stereo {
					"TRUE STEREO decoded"
				} else {
					"MONO decoded (L = R)"
				},
				stream.left_dbfs.unwrap_or(-120.0),
				stream.right_dbfs.unwrap_or(-120.0),
				stream.side_dbfs.unwrap_or(-120.0),
				stream.lr_correlation.unwrap_or(1.0),
			),
		};
		let fmtp = match stream.sdp_fmtp_stereo {
			Some(true) => "fmtp stereo=1",
			Some(false) => "fmtp mono decode",
			None => "fmtp unknown",
		};
		format!(
			"{send} · exact SSRC receive match · {} · {codec_channels} · {track_channels} · {fmtp} · {stereo} · {rate} kb/s · loss {loss:.2}% · jitter {jitter:.1} ms · conceal {} samples/s · discarded {} pkt/s · jitter buffer {:.1} ms · {energy}",
			stream.codec,
			stream.concealed_samples_per_second,
			stream.discarded_packets_delta,
			stream.jitter_buffer_delay_ms,
		)
	}
}

impl Drop for Interconnect {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::Release);
		if let Some(worker) = self.worker.take() {
			let _ = worker.join();
		}
	}
}

fn serve(mut stream: TcpStream, shared: &Arc<Mutex<Shared>>) {
	let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
	let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
	let mut reader = BufReader::new(&mut stream);
	let mut consumed = 0usize;
	let mut line = Vec::new();
	if read_line_bounded(&mut reader, &mut line, &mut consumed).is_err() {
		respond(
			&mut stream,
			431,
			"Request Header Fields Too Large",
			"{}",
			None,
		);
		return;
	}
	let request = String::from_utf8_lossy(&line);
	let mut parts = request.split_whitespace();
	let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
	else {
		respond(&mut stream, 400, "Bad Request", "{}", None);
		return;
	};
	if version != "HTTP/1.1" {
		respond(&mut stream, 400, "Bad Request", "{}", None);
		return;
	}
	let method = method.to_owned();
	let path = path.to_owned();
	let mut origin = None;
	let mut content_length = 0usize;
	loop {
		line.clear();
		if read_line_bounded(&mut reader, &mut line, &mut consumed).is_err() {
			respond(
				&mut stream,
				431,
				"Request Header Fields Too Large",
				"{}",
				None,
			);
			return;
		}
		if line == b"\r\n" || line == b"\n" {
			break;
		}
		let header = String::from_utf8_lossy(&line);
		if let Some((name, value)) = header.split_once(':') {
			match name.trim().to_ascii_lowercase().as_str() {
				"origin" => origin = Some(value.trim().to_owned()),
				"content-length" => {
					let Ok(value) = value.trim().parse::<usize>() else {
						respond(&mut stream, 400, "Bad Request", "{}", origin.as_deref());
						return;
					};
					content_length = value;
				}
				"transfer-encoding" => {
					respond(
						&mut stream,
						400,
						"Chunked Requests Not Accepted",
						"{}",
						origin.as_deref(),
					);
					return;
				}
				_ => {}
			}
		}
	}
	if origin.as_deref() != Some(EXTENSION_ORIGIN) {
		respond(&mut stream, 403, "Forbidden", "{}", None);
		return;
	}
	if method == "OPTIONS" {
		respond(&mut stream, 204, "No Content", "{}", origin.as_deref());
		return;
	}
	if content_length > MAX_BODY_BYTES {
		respond(
			&mut stream,
			413,
			"Payload Too Large",
			"{}",
			origin.as_deref(),
		);
		return;
	}
	match (method.as_str(), path.as_str()) {
		("GET", "/v1/session") => respond(
			&mut stream,
			200,
			"OK",
			&json!({"protocol": 1, "port": PORT}).to_string(),
			origin.as_deref(),
		),
		("GET", "/v1/status") => {
			let body = shared
				.lock()
				.map(|shared| {
					let fresh = shared
						.receiver_at
						.is_some_and(|at| at.elapsed() <= REPORT_MAX_AGE);
					let diagnostics_fresh = shared
						.extension_diagnostics_at
						.is_some_and(|at| at.elapsed() <= DIAGNOSTICS_MAX_AGE);
					json!({
						"protocol": 1,
						"sender": shared.sender,
						"receiver_connected": fresh,
						"receiver": if fresh { shared.receiver.clone() } else { None },
						"extension_diagnostics_connected": diagnostics_fresh,
						"extension_diagnostics": if diagnostics_fresh { shared.extension_diagnostics.clone() } else { None },
					})
				})
				.unwrap_or_else(
					|_| json!({"protocol": 1, "sender": null, "receiver_connected": false, "extension_diagnostics_connected": false}),
				);
			respond(&mut stream, 200, "OK", &body.to_string(), origin.as_deref());
		}
		("POST", "/v1/diagnostics") => {
			let mut body = vec![0u8; content_length];
			if reader.read_exact(&mut body).is_err() {
				respond(
					&mut stream,
					400,
					"Incomplete Request Body",
					"{}",
					origin.as_deref(),
				);
				return;
			}
			let Ok(diagnostics) = serde_json::from_slice::<ExtensionDiagnostics>(&body) else {
				respond(
					&mut stream,
					400,
					"Invalid Extension Diagnostics",
					"{}",
					origin.as_deref(),
				);
				return;
			};
			if !diagnostics.validate() {
				respond(
					&mut stream,
					400,
					"Extension Diagnostics Outside Limits",
					"{}",
					origin.as_deref(),
				);
				return;
			}
			if let Ok(mut shared) = shared.lock() {
				shared.extension_diagnostics = Some(diagnostics);
				shared.extension_diagnostics_at = Some(Instant::now());
			}
			respond(&mut stream, 204, "No Content", "{}", origin.as_deref());
		}
		("POST", "/v1/receiver") => {
			let mut body = vec![0u8; content_length];
			if reader.read_exact(&mut body).is_err() {
				respond(
					&mut stream,
					400,
					"Incomplete Request Body",
					"{}",
					origin.as_deref(),
				);
				return;
			}
			let Ok(report) = serde_json::from_slice::<ReceiverReport>(&body) else {
				respond(
					&mut stream,
					400,
					"Invalid Receiver Report",
					"{}",
					origin.as_deref(),
				);
				return;
			};
			if !report.validate() {
				respond(
					&mut stream,
					400,
					"Receiver Report Outside Limits",
					"{}",
					origin.as_deref(),
				);
				return;
			}
			if let Ok(mut shared) = shared.lock() {
				shared.receiver = Some(report);
				shared.receiver_at = Some(Instant::now());
			}
			respond(&mut stream, 204, "No Content", "{}", origin.as_deref());
		}
		_ => respond(&mut stream, 404, "Not Found", "{}", origin.as_deref()),
	}
}

fn read_line_bounded<R: BufRead>(
	reader: &mut R,
	line: &mut Vec<u8>,
	consumed: &mut usize,
) -> std::io::Result<()> {
	let max = MAX_HEADER_BYTES.saturating_sub(*consumed);
	if max == 0 {
		return Err(std::io::ErrorKind::InvalidData.into());
	}
	let count = reader.take(max as u64).read_until(b'\n', line)?;
	*consumed = (*consumed).saturating_add(count);
	if count == 0 || *consumed > MAX_HEADER_BYTES || line.last() != Some(&b'\n') {
		return Err(std::io::ErrorKind::InvalidData.into());
	}
	Ok(())
}

fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str, origin: Option<&str>) {
	let cors = origin.map_or_else(String::new, |origin| {
		format!("Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n")
	});
	let response = format!(
		"HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n{cors}\r\n{body}",
		body.len()
	);
	let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn receiver_limits_reject_nonfinite_and_excessive_values() {
		let report = ReceiverReport {
			protocol: 1,
			sampled_at_ms: 1,
			peer_connections: 1,
			streams: vec![InboundAudio {
				ssrc: Some(7),
				codec: "audio/opus".into(),
				channels: Some(2),
				track_channels: Some(2),
				sample_rate_hz: Some(48_000),
				bitrate_bps: 510_000,
				packets_received: 10,
				packets_lost: 0,
				loss_percent: 0.0,
				jitter_ms: 1.0,
				concealed_samples: 0,
				concealment_events: 0,
				discarded_packets: 0,
				audio_level: 0.2,
				active: true,
				concealed_samples_per_second: 0.0,
				concealment_events_delta: 0,
				discarded_packets_delta: 0,
				jitter_buffer_delay_ms: 0.0,
				sdp_fmtp_stereo: Some(true),
				left_dbfs: Some(-20.0),
				right_dbfs: Some(-20.0),
				side_dbfs: Some(-23.0),
				lr_correlation: Some(0.02),
			}],
		};
		assert!(report.validate());
		assert_eq!(report.streams[0].stereo_verdict(), StereoVerdict::Stereo);
		let mut mono = report.clone();
		mono.streams[0].side_dbfs = Some(-110.0);
		mono.streams[0].lr_correlation = Some(1.0);
		assert_eq!(mono.streams[0].stereo_verdict(), StereoVerdict::Mono);
		mono.streams[0].left_dbfs = Some(-90.0);
		mono.streams[0].right_dbfs = Some(-90.0);
		assert_eq!(mono.streams[0].stereo_verdict(), StereoVerdict::Silent);
		let mut invalid = report;
		invalid.streams[0].jitter_ms = f32::NAN;
		assert!(!invalid.validate());
	}

	#[test]
	fn reports_from_the_previous_extension_version_still_parse() {
		let old = r#"{"protocol":1,"sampled_at_ms":1,"peer_connections":1,"streams":[{"ssrc":7,"codec":"audio/opus","channels":2,"track_channels":null,"sample_rate_hz":48000,"bitrate_bps":1,"packets_received":1,"packets_lost":0,"loss_percent":0,"jitter_ms":0,"concealed_samples":0,"concealment_events":0,"discarded_packets":0,"audio_level":0,"active":false,"concealed_samples_per_second":0,"concealment_events_delta":0,"discarded_packets_delta":0,"jitter_buffer_delay_ms":0}]}"#;
		let report: ReceiverReport = serde_json::from_str(old).unwrap();
		assert!(report.validate());
		assert_eq!(
			report.streams[0].stereo_verdict(),
			StereoVerdict::Unmeasured
		);
	}

	#[test]
	fn extension_diagnostics_are_bounded_and_privacy_limited() {
		let valid = ExtensionDiagnostics {
			protocol: 1,
			sampled_at_ms: 1,
			content_bridge: true,
			observer_state: "hook-installed".into(),
			observer_api: true,
			observer_error: String::new(),
			peer_connections: 2,
			inbound_streams: 1,
			report_count: 15,
			forward_error: String::new(),
			sweep_test: None,
			sweep_curve: None,
		};
		assert!(valid.validate());
		let value = serde_json::to_value(&valid).unwrap();
		assert!(value.get("audio_data").is_none());
		assert!(value.get("token").is_none());
		assert!(value.get("account").is_none());
		let mut invalid = valid;
		invalid.observer_error = "x".repeat(181);
		assert!(!invalid.validate());
	}

	#[test]
	fn sweep_curve_is_bounded_and_keeps_only_the_matched_sender() {
		let mut diagnostics = ExtensionDiagnostics {
			protocol: 1,
			sampled_at_ms: 1,
			content_bridge: true,
			observer_state: "sampling".into(),
			observer_api: true,
			observer_error: String::new(),
			peer_connections: 1,
			inbound_streams: 1,
			report_count: 1,
			forward_error: String::new(),
			sweep_test: Some(SweepTestDiagnostics {
				running: false,
				automatic: true,
				capture_active: false,
				export_ready: true,
				sender_ssrc: Some(42),
				sample_count: 20,
				matched_samples: 20,
				average_loss_percent: Some(0.0),
				peak_loss_percent: Some(0.0),
				average_jitter_ms: Some(1.0),
				peak_jitter_ms: Some(2.0),
			}),
			sweep_curve: Some(SweepCurveDiagnostics {
				ssrc: 42,
				spectrum_dbfs: vec![-18.0; 48],
			}),
		};
		assert!(diagnostics.validate());
		diagnostics
			.sweep_curve
			.as_mut()
			.unwrap()
			.spectrum_dbfs
			.pop();
		assert!(!diagnostics.validate());
		diagnostics.sweep_curve.as_mut().unwrap().spectrum_dbfs = vec![-18.0; 48];
		diagnostics.sweep_curve.as_mut().unwrap().ssrc = 43;
		assert!(!diagnostics.validate());
	}

	#[test]
	fn serialized_report_contains_no_media_or_account_fields() {
		let report = ReceiverReport {
			protocol: 1,
			sampled_at_ms: 1,
			peer_connections: 0,
			streams: Vec::new(),
		};
		let value = serde_json::to_value(report).unwrap();
		assert!(value.get("token").is_none());
		assert!(value.get("account").is_none());
		assert!(value.get("audio_data").is_none());
	}
}
