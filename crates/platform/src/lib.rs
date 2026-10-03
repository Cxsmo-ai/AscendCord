//! Credential persistence and temporary owner-operated login/verification surfaces.
pub mod badge;
pub mod captcha;
pub mod compositor;
pub mod game_activity;
pub mod gpu;
pub mod hotkeys;
pub mod notifications;
pub mod pointer;
pub mod processes;
pub mod save;
pub mod startup;
pub mod tray;
pub mod urls;
pub mod video;
#[cfg(target_os = "macos")]
pub mod window;
pub mod window_effects;
use client_core::auth::{Failure, SessionSecret};
pub use pointer::cursor_position;
#[cfg(not(target_os = "linux"))]
use std::{
	sync::{
		Arc,
		mpsc::{self, Receiver},
	},
	time::{Duration, Instant},
};
#[cfg(not(target_os = "linux"))]
use wry::{WebView, WebViewBuilder};
#[cfg(target_os = "linux")]
mod login_linux;
#[cfg(target_os = "linux")]
pub use login_linux::LoginView;

/// Logical height of the native header the desktop app draws above the login webview.
pub const LOGIN_HEADER_HEIGHT: f32 = 56.0;
/// Desktop identity: Linux GApplication id, Windows AUMID, Wayland app id.
pub const APP_ID: &str = "org.ascendcord.AscendCord";
/// OS credential store service. Separate from [`APP_ID`] so each can change on its own.
const SERVICE: &str = "org.ascendcord.AscendCord";
/// Service used before the AscendCord rename; read and copied, never written or deleted.
const LEGACY_SERVICE: &str = "org.testcord.tesktop2-native";
const ACCOUNT: &str = "discord-session";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialError {
	/// No OS credential store exists (e.g. Linux without a Secret Service provider).
	NoStore,
	Unavailable,
	Invalid,
	TimedOut,
}

#[cfg(target_os = "linux")]
pub(crate) fn ensure_gtk_application_id() {
	static INIT: std::sync::Once = std::sync::Once::new();
	INIT.call_once(|| {
		use gtk4::gio::prelude::ApplicationExt;
		let app = gtk4::gio::Application::new(Some(APP_ID), gtk4::gio::ApplicationFlags::empty());
		if let Err(error) = app.register(gtk4::gio::Cancellable::NONE) {
			eprintln!("Linux login/verification: GApplication registration failed: {error}");
		}
		std::mem::forget(app);
	});
}

/// The entry restored on launch. Switching accounts rewrites it from the per-account entry.
pub fn load_session() -> Result<Option<SessionSecret>, CredentialError> {
	load_entry(ACCOUNT)
}
pub fn save_session(secret: &SessionSecret) -> Result<(), CredentialError> {
	save_entry(ACCOUNT, secret)
}
pub fn forget_session() -> Result<(), CredentialError> {
	forget_entry(ACCOUNT)
}
/// One entry per remembered account, so the switcher never keeps a second copy in memory.
fn account_entry(account: model::Id) -> String {
	format!("{ACCOUNT}.{account}")
}
pub fn load_account_session(account: model::Id) -> Result<Option<SessionSecret>, CredentialError> {
	load_entry(&account_entry(account))
}
pub fn save_account_session(
	account: model::Id,
	secret: &SessionSecret,
) -> Result<(), CredentialError> {
	save_entry(&account_entry(account), secret)
}
pub fn forget_account_session(account: model::Id) -> Result<(), CredentialError> {
	forget_entry(&account_entry(account))
}
fn entry_for(service: &str, name: &str) -> Result<keyring::Entry, CredentialError> {
	keyring::Entry::new(service, name).map_err(|error| match error {
		keyring::Error::NoDefaultStore => CredentialError::NoStore,
		_ => CredentialError::Unavailable,
	})
}
fn load_entry(name: &str) -> Result<Option<SessionSecret>, CredentialError> {
	vault::load(&vault::Os, name)?
		.map(|value| SessionSecret::from_owner_input(value).map_err(|_| CredentialError::Invalid))
		.transpose()
}
fn save_entry(name: &str, secret: &SessionSecret) -> Result<(), CredentialError> {
	vault::save(&vault::Os, name, secret.expose())
}
fn forget_entry(name: &str) -> Result<(), CredentialError> {
	vault::forget(&vault::Os, name)
}

/// Credential policy across the AscendCord and pre-rename services.
mod vault {
	use super::{CredentialError, LEGACY_SERVICE, SERVICE};

	pub(super) trait Store {
		fn get(&self, service: &str, name: &str) -> Result<Option<String>, CredentialError>;
		fn set(&self, service: &str, name: &str, value: &str) -> Result<(), CredentialError>;
		fn delete(&self, service: &str, name: &str) -> Result<(), CredentialError>;
	}

	/// The operating system's credential store.
	pub(super) struct Os;
	impl Store for Os {
		fn get(&self, service: &str, name: &str) -> Result<Option<String>, CredentialError> {
			match super::entry_for(service, name)?.get_password() {
				Ok(value) => Ok(Some(value)),
				Err(keyring::Error::NoEntry) => Ok(None),
				Err(_) => Err(CredentialError::Unavailable),
			}
		}
		fn set(&self, service: &str, name: &str, value: &str) -> Result<(), CredentialError> {
			super::entry_for(service, name)?
				.set_password(value)
				.map_err(|_| CredentialError::Unavailable)
		}
		fn delete(&self, service: &str, name: &str) -> Result<(), CredentialError> {
			match super::entry_for(service, name)?.delete_credential() {
				Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
				Err(_) => Err(CredentialError::Unavailable),
			}
		}
	}

	/// Written on sign-out so the untouched legacy entry is not used again.
	fn tombstone(name: &str) -> String {
		format!("legacy-fallback-disabled.{name}")
	}

	/// The AscendCord entry, else (unless signed out since) the pre-rename one, copied over.
	pub(super) fn load(store: &impl Store, name: &str) -> Result<Option<String>, CredentialError> {
		if let Some(value) = store.get(SERVICE, name)? {
			return Ok(Some(value));
		}
		if store.get(SERVICE, &tombstone(name))?.is_some() {
			return Ok(None);
		}
		let Some(value) = store.get(LEGACY_SERVICE, name)? else {
			return Ok(None);
		};
		// A failed copy only means the next launch reads the legacy entry again.
		let _ = store.set(SERVICE, name, &value);
		Ok(Some(value))
	}
	pub(super) fn save(store: &impl Store, name: &str, value: &str) -> Result<(), CredentialError> {
		store.set(SERVICE, name, value)?;
		let _ = store.delete(SERVICE, &tombstone(name));
		Ok(())
	}
	pub(super) fn forget(store: &impl Store, name: &str) -> Result<(), CredentialError> {
		// Written before the delete and without reading the legacy service, so no partial
		// failure can leave the legacy entry able to sign the user back in.
		store.set(SERVICE, &tombstone(name), "1")?;
		store.delete(SERVICE, name)
	}

	#[cfg(test)]
	mod tests {
		use super::*;
		use std::{cell::RefCell, collections::BTreeMap};

		#[derive(Default)]
		struct Memory {
			entries: RefCell<BTreeMap<(String, String), String>>,
			readonly: bool,
			/// Simulates an unavailable legacy service.
			legacy_down: std::cell::Cell<bool>,
		}
		impl Store for Memory {
			fn get(&self, service: &str, name: &str) -> Result<Option<String>, CredentialError> {
				if service == LEGACY_SERVICE && self.legacy_down.get() {
					return Err(CredentialError::Unavailable);
				}
				Ok(self
					.entries
					.borrow()
					.get(&(service.into(), name.into()))
					.cloned())
			}
			fn set(&self, service: &str, name: &str, value: &str) -> Result<(), CredentialError> {
				if self.readonly {
					return Err(CredentialError::Unavailable);
				}
				self.entries
					.borrow_mut()
					.insert((service.into(), name.into()), value.into());
				Ok(())
			}
			fn delete(&self, service: &str, name: &str) -> Result<(), CredentialError> {
				self.entries
					.borrow_mut()
					.remove(&(service.into(), name.into()));
				Ok(())
			}
		}
		fn legacy(store: &Memory, name: &str, value: &str) {
			store
				.entries
				.borrow_mut()
				.insert((LEGACY_SERVICE.into(), name.into()), value.into());
		}

		#[test]
		fn legacy_sessions_are_copied_lazily_and_kept() {
			let store = Memory::default();
			legacy(&store, "discord-session.42", "old");
			assert_eq!(
				load(&store, "discord-session.42").unwrap().as_deref(),
				Some("old")
			);
			assert_eq!(
				store.get(SERVICE, "discord-session.42").unwrap().as_deref(),
				Some("old")
			);
			assert!(
				store
					.get(LEGACY_SERVICE, "discord-session.42")
					.unwrap()
					.is_some()
			);
			assert_eq!(load(&store, "discord-session").unwrap(), None);
		}

		#[test]
		fn the_new_entry_wins_and_a_failed_copy_still_signs_in() {
			let store = Memory::default();
			legacy(&store, "discord-session", "old");
			save(&store, "discord-session", "new").unwrap();
			assert_eq!(
				load(&store, "discord-session").unwrap().as_deref(),
				Some("new")
			);
			let readonly = Memory {
				readonly: true,
				..Default::default()
			};
			legacy(&readonly, "discord-session", "old");
			assert_eq!(
				load(&readonly, "discord-session").unwrap().as_deref(),
				Some("old")
			);
		}

		#[test]
		fn sign_out_holds_even_if_the_legacy_service_fails_meanwhile() {
			let store = Memory::default();
			legacy(&store, "discord-session", "old");
			assert!(load(&store, "discord-session").unwrap().is_some());
			store.legacy_down.set(true);
			forget(&store, "discord-session").unwrap();
			store.legacy_down.set(false);
			assert_eq!(load(&store, "discord-session").unwrap(), None);
		}

		#[test]
		fn sign_out_stays_signed_out_without_deleting_the_legacy_entry() {
			let store = Memory::default();
			legacy(&store, "discord-session", "old");
			assert!(load(&store, "discord-session").unwrap().is_some());
			forget(&store, "discord-session").unwrap();
			assert_eq!(load(&store, "discord-session").unwrap(), None);
			assert_eq!(
				store
					.get(LEGACY_SERVICE, "discord-session")
					.unwrap()
					.as_deref(),
				Some("old")
			);
			// Signing in again clears the tombstone.
			save(&store, "discord-session", "fresh").unwrap();
			assert_eq!(
				load(&store, "discord-session").unwrap().as_deref(),
				Some("fresh")
			);
			forget(&store, "discord-session").unwrap();
			assert_eq!(load(&store, "discord-session").unwrap(), None);
		}
	}
}
fn discord_origin(value: &str) -> bool {
	url::Url::parse(value).is_ok_and(|url| {
		url.scheme() == "https"
			&& url.host_str() == Some("discord.com")
			&& url.port_or_known_default() == Some(443)
			&& url.username().is_empty()
			&& url.password().is_none()
	})
}
#[cfg(any(not(target_os = "linux"), test))]
fn login_navigation(value: &str) -> bool {
	discord_origin(value) || captcha::hcaptcha_origin(value)
}
/// Receives only the account token used by THIS ephemeral, owner-operated login page.
/// No browser-profile reads, password interception, console instructions, or QR exchange implementation.
#[cfg(not(target_os = "linux"))]
pub struct LoginView {
	view: WebView,
	tokens: Receiver<SessionSecret>,
	opened: Instant,
}
#[cfg(not(target_os = "linux"))]
impl LoginView {
	pub fn open(
		parent: Arc<winit::window::Window>,
		wake: impl Fn() + Send + Sync + 'static,
	) -> Result<Self, Failure> {
		let (send, tokens) = mpsc::sync_channel(1);
		let mut random = [0_u8; 32];
		getrandom::fill(&mut random).map_err(|_| Failure::Protocol)?;
		let capability = random
			.iter()
			.map(|byte| format!("{byte:02x}"))
			.collect::<String>()
			+ ":";
		let script = include_str!("login-handoff.js")
			.replace("__ASCENDCORD_LOGIN_CAPABILITY__", &capability);
		let builder = WebViewBuilder::new()
			.with_url("https://discord.com/login")
			.with_incognito(true)
			.with_devtools(false)
			.with_initialization_script_for_main_only(script, true)
			.with_navigation_handler(|url| login_navigation(&url))
			.with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
			.with_download_started_handler(|_, _| false)
			.with_ipc_handler(move |request| {
				if !discord_origin(&request.uri().to_string()) || request.body().len() > 2113 {
					return;
				}
				let body = zeroize::Zeroizing::new(request.into_body());
				let Some(value) = body.strip_prefix(&capability) else {
					return;
				};
				if let Ok(secret) = SessionSecret::from_owner_input(value.to_owned()) {
					let _ = send.try_send(secret);
					wake();
				}
			});
		let view = builder
			.with_bounds(bounds(&parent))
			.build_as_child(parent.as_ref())
			.map_err(|_| Failure::Protocol)?;
		Ok(Self {
			view,
			tokens,
			opened: Instant::now(),
		})
	}
	pub fn token(&self) -> Option<SessionSecret> {
		self.tokens.try_recv().ok()
	}
	pub fn expired(&self) -> bool {
		self.opened.elapsed() > Duration::from_secs(600)
	}
	pub fn crashed(&self) -> bool {
		false
	}
	pub fn resize(&self, parent: &winit::window::Window) {
		let _ = self.view.set_bounds(bounds(parent));
	}
	pub fn pump(&self) {}
}
#[cfg(not(target_os = "linux"))]
fn bounds(parent: &winit::window::Window) -> wry::Rect {
	let size = parent.inner_size();
	let header = (LOGIN_HEADER_HEIGHT as f64 * parent.scale_factor()).round() as u32;
	wry::Rect {
		position: wry::dpi::PhysicalPosition::new(0, header as i32).into(),
		size: wry::dpi::PhysicalSize::new(size.width, size.height.saturating_sub(header)).into(),
	}
}
#[cfg(test)]
mod tests {
	use super::*;
	#[test]
	fn handoff_accepts_only_our_discord_origin() {
		assert!(discord_origin("https://discord.com/login"));
		for value in [
			"http://discord.com",
			"https://discord.com.evil.test",
			"https://evil.test/discord.com",
			"https://user@discord.com",
			"https://discord.com:444",
		] {
			assert!(!discord_origin(value));
		}
		let script = include_str!("login-handoff.js");
		assert!(!script.contains("localStorage"));
		assert!(!script.contains("password"));
	}
	#[test]
	fn login_allows_hcaptcha_frames_only_over_https() {
		assert!(login_navigation("https://newassets.hcaptcha.com/captcha/"));
		assert!(!login_navigation("http://hcaptcha.com/"));
		assert!(!login_navigation("https://hcaptcha.com.evil.test/"));
	}
}
