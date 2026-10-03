//! App icon styles, all free to choose. Each style has bundled artwork under
//! `assets/brand/icon-styles`, named by its slug.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppIcon {
	/// AscendCord's own icon. Also used for styles saved by a newer version.
	#[default]
	Sakura,
	/// The icon from before the AscendCord name.
	Classic,
	Blurple,
	Goth,
	Wham,
	Charcoal,
	Ceramic,
	Pastel,
	Matey,
	Tactical,
	SunsetAve,
	GalacticChrome,
	Holo,
	SherbetDreamsicle,
	Gaming,
	Mainframe,
	PrismaticWaves,
	Uwu,
	Fuming,
}

impl AppIcon {
	/// Picker order.
	pub const ALL: [Self; 19] = [
		Self::Sakura,
		Self::Classic,
		Self::Blurple,
		Self::Goth,
		Self::Wham,
		Self::Charcoal,
		Self::Ceramic,
		Self::Pastel,
		Self::Matey,
		Self::Tactical,
		Self::SunsetAve,
		Self::GalacticChrome,
		Self::Holo,
		Self::SherbetDreamsicle,
		Self::Gaming,
		Self::Mainframe,
		Self::PrismaticWaves,
		Self::Uwu,
		Self::Fuming,
	];

	/// File name stem of the style's artwork.
	pub const fn slug(self) -> &'static str {
		match self {
			Self::Classic => "classic",
			Self::Sakura => "sakura",
			Self::Blurple => "blurple",
			Self::Goth => "goth",
			Self::Wham => "wham",
			Self::Charcoal => "charcoal",
			Self::Ceramic => "ceramic",
			Self::Pastel => "pastel",
			Self::Matey => "matey",
			Self::Tactical => "tactical",
			Self::SunsetAve => "sunset-ave",
			Self::GalacticChrome => "galactic-chrome",
			Self::Holo => "holo",
			Self::SherbetDreamsicle => "sherbet-dreamsicle",
			Self::Gaming => "gaming",
			Self::Mainframe => "mainframe",
			Self::PrismaticWaves => "prismatic-waves",
			Self::Uwu => "uwu",
			Self::Fuming => "fuming",
		}
	}

	pub const fn label(self) -> &'static str {
		match self {
			Self::Classic => "Classic",
			Self::Sakura => "Sakura",
			Self::Blurple => "Blurple",
			Self::Goth => "Goth",
			Self::Wham => "WHAM",
			Self::Charcoal => "Charcoal",
			Self::Ceramic => "Ceramic",
			Self::Pastel => "Pastel",
			Self::Matey => "Matey",
			Self::Tactical => "Tactical",
			Self::SunsetAve => "Sunset Ave",
			Self::GalacticChrome => "Galactic Chrome",
			Self::Holo => "Holo",
			Self::SherbetDreamsicle => "Sherbet Dreamsicle",
			Self::Gaming => "Gaming",
			Self::Mainframe => "Mainframe",
			Self::PrismaticWaves => "Prismatic Waves",
			Self::Uwu => "uwu~",
			Self::Fuming => "Fuming",
		}
	}
}

impl<'de> serde::Deserialize<'de> for AppIcon {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let slug = std::borrow::Cow::<str>::deserialize(deserializer)?;
		Ok(Self::ALL
			.into_iter()
			.find(|icon| icon.slug() == slug)
			.unwrap_or_default())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn slugs_round_trip_and_unknown_styles_fall_back_to_classic() {
		for icon in AppIcon::ALL {
			let json = serde_json::to_string(&icon).unwrap();
			assert_eq!(json, format!("\"{}\"", icon.slug()));
			assert_eq!(serde_json::from_str::<AppIcon>(&json).unwrap(), icon);
		}
		assert_eq!(
			serde_json::from_str::<AppIcon>("\"future-style\"").unwrap(),
			AppIcon::Sakura
		);
		let mut slugs: Vec<_> = AppIcon::ALL.iter().map(|icon| icon.slug()).collect();
		slugs.sort_unstable();
		slugs.dedup();
		assert_eq!(slugs.len(), AppIcon::ALL.len());
	}
}
