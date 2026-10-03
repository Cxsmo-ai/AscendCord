use super::Event;
use ksni::{TrayMethods, menu::StandardItem};
use std::sync::{
	Arc,
	atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tokio::sync::oneshot;

struct Events {
	bits: AtomicU8,
	// Pending -> ready, or permanently unavailable until this registration is replaced.
	availability: AtomicU8,
	wake: Box<dyn Fn() + Send + Sync>,
	restore: Box<dyn Fn() + Send + Sync>,
}

impl Events {
	fn push(&self, event: Event) {
		if event == Event::Unavailable {
			self.availability.store(2, Ordering::Release);
		}
		// A hidden window may receive no frames, so the UI would never see this event.
		if event != Event::Minimize {
			(self.restore)();
		}
		if self.bits.fetch_or(event as u8, Ordering::Relaxed) & event as u8 == 0 {
			(self.wake)();
		}
	}
}

/// One cancellable registration on the application's runtime, never a UI-thread D-Bus call.
pub struct Tray {
	events: Arc<Events>,
	_stop: oneshot::Sender<()>,
	icon: tokio::sync::watch::Sender<Option<&'static [u8]>>,
}

impl Tray {
	/// `restore` runs on the tray worker for events that need a visible window, before `wake`.
	pub fn new(
		wake: impl Fn() + Send + Sync + 'static,
		restore: impl Fn() + Send + Sync + 'static,
	) -> Result<Self, &'static str> {
		let runtime = tokio::runtime::Handle::try_current()
			.map_err(|_| "The tray requires the application runtime.")?;
		let events = Arc::new(Events {
			bits: AtomicU8::new(0),
			availability: AtomicU8::new(0),
			wake: Box::new(wake),
			restore: Box::new(restore),
		});
		let (stop, mut stopped) = oneshot::channel();
		let (icon, mut icon_changes) = tokio::sync::watch::channel(None::<&'static [u8]>);
		let worker_events = events.clone();
		runtime.spawn(async move {
			let Some(pixmap) = decode_pixmap(include_bytes!(
				"../../../../packaging/linux/hicolor/32x32/apps/ascendcord.png"
			)) else {
				worker_events.push(Event::Unavailable);
				return;
			};
			let item = Item {
				events: worker_events.clone(),
				pixmap,
			};
			let registration = item.disable_dbus_name(true).spawn();
			let result = tokio::select! {
				_ = &mut stopped => return,
				result = tokio::time::timeout(Duration::from_secs(3), registration) => result,
			};
			let Ok(Ok(handle)) = result else {
				worker_events.push(Event::Unavailable);
				return;
			};
			// ksni subscribes to watcher changes after registration. Recheck after
			// subscribing so a host lost during registration cannot leave a phantom tray.
			let host_present = tokio::select! {
				_ = &mut stopped => false,
				result = tokio::time::timeout(Duration::from_secs(3), async {
					let bus = zbus::Connection::session().await?;
					let dbus = zbus::fdo::DBusProxy::new(&bus).await?;
					dbus.name_has_owner("org.kde.StatusNotifierWatcher".try_into().unwrap()).await
				}) => matches!(result, Ok(Ok(true))),
			};
			if !host_present || handle.is_closed() {
				worker_events.push(Event::Unavailable);
				let _ = tokio::time::timeout(Duration::from_secs(2), handle.shutdown()).await;
				return;
			}
			if worker_events
				.availability
				.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
				.is_ok()
			{
				(worker_events.wake)();
			}
			loop {
				tokio::select! {
					_ = &mut stopped => break,
					changed = icon_changes.changed() => {
						if changed.is_err() {
							break;
						}
						let png = *icon_changes.borrow_and_update();
						if let Some(pixmap) = png.and_then(decode_pixmap) {
							let _ = handle.update(move |item: &mut Item| item.pixmap = pixmap).await;
						}
					}
				}
			}
			let _ = tokio::time::timeout(Duration::from_secs(2), handle.shutdown()).await;
		});
		Ok(Self {
			events,
			_stop: stop,
			icon,
		})
	}
	/// Show another app icon style (a PNG); applied once the tray is registered.
	pub fn set_icon(&self, png: &'static [u8]) {
		self.icon.send_replace(Some(png));
	}
	pub fn is_available(&self) -> bool {
		self.events.availability.load(Ordering::Acquire) == 1
	}

	pub fn take_event(&self) -> Option<Event> {
		[
			Event::Quit,
			Event::Minimize,
			Event::Unavailable,
			Event::Show,
		]
		.into_iter()
		.find(|event| {
			self.events
				.bits
				.fetch_and(!(*event as u8), Ordering::Relaxed)
				& *event as u8
				!= 0
		})
	}
}

struct Item {
	events: Arc<Events>,
	pixmap: ksni::Icon,
}

/// Decode a PNG into a StatusNotifier pixmap, which is ARGB rather than RGBA.
fn decode_pixmap(png: &[u8]) -> Option<ksni::Icon> {
	let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
		.ok()?
		.into_rgba8();
	let (width, height) = image.dimensions();
	let mut data = image.into_raw();
	for pixel in data.as_chunks_mut::<4>().0 {
		pixel.rotate_right(1);
	}
	Some(ksni::Icon {
		width: width as i32,
		height: height as i32,
		data,
	})
}

impl ksni::Tray for Item {
	fn id(&self) -> String {
		"ascendcord".into()
	}
	fn title(&self) -> String {
		"AscendCord".into()
	}
	fn icon_pixmap(&self) -> Vec<ksni::Icon> {
		vec![self.pixmap.clone()]
	}
	fn activate(&mut self, _x: i32, _y: i32) {
		self.events.push(Event::Show);
	}
	fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
		vec![
			StandardItem {
				label: "Show AscendCord".into(),
				activate: Box::new(|item: &mut Self| item.events.push(Event::Show)),
				..Default::default()
			}
			.into(),
			StandardItem {
				label: "Minimize AscendCord".into(),
				activate: Box::new(|item: &mut Self| item.events.push(Event::Minimize)),
				..Default::default()
			}
			.into(),
			StandardItem {
				label: "Quit".into(),
				activate: Box::new(|item: &mut Self| item.events.push(Event::Quit)),
				..Default::default()
			}
			.into(),
		]
	}
	fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
		self.events.push(Event::Unavailable);
		false
	}
}
