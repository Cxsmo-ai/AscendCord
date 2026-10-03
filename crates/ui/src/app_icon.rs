//! Bundled app icon artwork and the process-wide choice the brand mark follows.
use model::AppIcon;
use std::sync::atomic::{AtomicU8, Ordering};

static CURRENT: AtomicU8 = AtomicU8::new(0);

macro_rules! styles {
	($size:literal: $($icon:ident => $slug:literal),* $(,)?) => {
		|icon: AppIcon| -> &'static [u8] {
			match icon {
				AppIcon::Classic => include_bytes!(concat!(
					"../../../assets/brand/icon-styles/", $size, "/classic.png"
				)),
				$(AppIcon::$icon => include_bytes!(concat!(
					"../../../assets/brand/icon-styles/", $size, "/", $slug, ".png"
				)),)*
			}
		}
	};
}

/// 256 px PNG used for the window, taskbar and in-app brand mark.
pub fn png(icon: AppIcon) -> &'static [u8] {
	let bytes = styles!("256":
		Sakura => "sakura", Blurple => "blurple", Goth => "goth", Wham => "wham",
		Charcoal => "charcoal", Ceramic => "ceramic", Pastel => "pastel", Matey => "matey",
		Tactical => "tactical", SunsetAve => "sunset-ave", GalacticChrome => "galactic-chrome",
		Holo => "holo", SherbetDreamsicle => "sherbet-dreamsicle", Gaming => "gaming",
		Mainframe => "mainframe", PrismaticWaves => "prismatic-waves", Uwu => "uwu",
		Fuming => "fuming",
	);
	bytes(icon)
}

/// 64 px PNG for the picker and tray.
pub fn thumbnail(icon: AppIcon) -> &'static [u8] {
	let bytes = styles!("64":
		Sakura => "sakura", Blurple => "blurple", Goth => "goth", Wham => "wham",
		Charcoal => "charcoal", Ceramic => "ceramic", Pastel => "pastel", Matey => "matey",
		Tactical => "tactical", SunsetAve => "sunset-ave", GalacticChrome => "galactic-chrome",
		Holo => "holo", SherbetDreamsicle => "sherbet-dreamsicle", Gaming => "gaming",
		Mainframe => "mainframe", PrismaticWaves => "prismatic-waves", Uwu => "uwu",
		Fuming => "fuming",
	);
	bytes(icon)
}

/// The style the in-app brand mark shows.
pub fn current() -> AppIcon {
	AppIcon::ALL
		.get(usize::from(CURRENT.load(Ordering::Relaxed)))
		.copied()
		.unwrap_or_default()
}

pub fn set_current(icon: AppIcon) {
	let index = AppIcon::ALL
		.iter()
		.position(|item| *item == icon)
		.unwrap_or(0);
	CURRENT.store(index as u8, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_saved_style_reaches_the_brand_mark_without_opening_settings() {
		let mut view = crate::MessagingUi {
			app_icon: AppIcon::Goth,
			..Default::default()
		};
		// The per-frame publish, as after preferences load at start-up.
		view.sync_streamer_mode();
		assert_eq!(current(), AppIcon::Goth);
		set_current(AppIcon::default());
	}

	#[test]
	fn every_style_has_decodable_square_artwork_with_transparent_corners() {
		for icon in AppIcon::ALL {
			for (bytes, size) in [(png(icon), 256), (thumbnail(icon), 64)] {
				let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
					.unwrap_or_else(|_| panic!("{} {size}", icon.slug()))
					.into_rgba8();
				assert_eq!(image.dimensions(), (size, size), "{}", icon.slug());
				if icon != AppIcon::Classic {
					assert_eq!(image.get_pixel(0, 0)[3], 0, "{} corner", icon.slug());
				}
			}
		}
	}
}
