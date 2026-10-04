//! Converts UTC instants to the user's local zone for display.
use crate::testcord::HourFormat;
#[cfg(not(test))]
use std::{cell::RefCell, collections::HashMap};

#[cfg(not(test))]
thread_local! {
	/// Quarter-hour offsets avoid querying the OS once per visible message while handling
	/// modern time-zone changes that occur on the half hour.
	static OFFSET_CACHE: RefCell<HashMap<i64, time::UtcOffset>> = RefCell::new(HashMap::new());
}

/// What the bundled ports changed about clocks and markers. `Default` is the client's own look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Display {
	/// Round relative phrases down instead of to the nearest.
	pub floor_relative: bool,
	pub hour: HourFormat,
	/// Minutes to shift every clock by, within a real time zone.
	pub offset_minutes: i32,
	pub hide_edited: bool,
	/// Keep messages from being marked as read while they are on screen.
	pub hold_read_ack: bool,
	/// Keep the body of a deleted message so it can still be read.
	pub preserve_deleted: bool,
	/// The composer's counter, or `None` for the app's own near-limit counter.
	pub counter: Option<Counter>,
	/// Count the words and characters under every message long enough to be worth counting.
	pub word_count: bool,
}

/// How the composer's character counter reads. Mirrors the runtime's own enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter {
	/// Show it from the first character instead of near the limit.
	pub always: bool,
	/// Follow the percentage thresholds TestCord uses.
	pub colors: bool,
}

/// The clock a message row shows, with the owner's own format and offset applied.
pub fn clock(instant: time::OffsetDateTime, display: &Display) -> String {
	let at = displayed_local(instant, display);
	match display.hour {
		HourFormat::Keep | HourFormat::TwentyFour => format!("{:02}:{:02}", at.hour(), at.minute()),
		HourFormat::Twelve => {
			let twelve = match at.hour() % 12 {
				0 => 12,
				hour => hour,
			};
			let suffix = if at.hour() < 12 { "AM" } else { "PM" };
			format!("{twelve}:{:02} {suffix}", at.minute())
		}
	}
}

/// Full, localized context for a message timestamp. The short clock stays beside the
/// author (or in the compact-row gutter); this is available on hover for the exact date.
pub fn message_tooltip(instant: time::OffsetDateTime, display: &Display) -> String {
	let at = displayed_local(instant, display);
	format!(
		"{} {}, {} · {}",
		at.month(),
		at.day(),
		at.year(),
		clock(instant, display)
	)
}

fn displayed_local(instant: time::OffsetDateTime, display: &Display) -> time::OffsetDateTime {
	local(instant) + time::Duration::minutes(i64::from(display.offset_minutes))
}

/// Shifts `instant` to the local offset in effect at that moment; falls back to UTC.
pub fn local(instant: time::OffsetDateTime) -> time::OffsetDateTime {
	// Tests pin UTC so date-boundary assertions hold on every machine.
	#[cfg(test)]
	let offset = time::UtcOffset::UTC;
	#[cfg(not(test))]
	let offset = {
		let quarter_hour = instant.unix_timestamp().div_euclid(900);
		OFFSET_CACHE.with(|cache| {
			if let Some(offset) = cache.borrow().get(&quarter_hour).copied() {
				return offset;
			}
			let offset = time::UtcOffset::local_offset_at(instant).unwrap_or(time::UtcOffset::UTC);
			let mut cache = cache.borrow_mut();
			if cache.len() >= 512 {
				cache.clear();
			}
			cache.insert(quarter_hour, offset);
			offset
		})
	};
	instant.to_offset(offset)
}

/// Current wall-clock time in the local zone.
pub fn now() -> time::OffsetDateTime {
	local(time::OffsetDateTime::now_utc())
}

/// Renders a Discord `<t:seconds:style>` reference the way the official client does: absolute
/// styles in the viewer's zone, `R` as a coarse relative phrase that refreshes on every frame.
pub fn discord_timestamp(seconds: i64, style: u8) -> Option<String> {
	let instant = time::OffsetDateTime::from_unix_timestamp(seconds).ok()?;
	if style == b'R' {
		return Some(relative(instant - time::OffsetDateTime::now_utc()));
	}
	let at = local(instant);
	let date = at.date();
	let clock = format!("{:02}:{:02}", at.hour(), at.minute());
	Some(match style {
		b't' => clock,
		b'T' => format!("{clock}:{:02}", at.second()),
		b'd' => format!(
			"{:02}/{:02}/{}",
			u8::from(date.month()),
			date.day(),
			date.year()
		),
		b'D' => format!("{} {}, {}", date.month(), date.day(), date.year()),
		b'F' => format!(
			"{}, {} {}, {} {clock}",
			date.weekday(),
			date.month(),
			date.day(),
			date.year()
		),
		_ => format!("{} {}, {} {clock}", date.month(), date.day(), date.year()),
	})
}
/// "… ago" for a past instant, measured against the current clock.
pub(crate) fn ago(instant: time::OffsetDateTime) -> String {
	ago_with(instant, &Display::default())
}

pub(crate) fn ago_with(instant: time::OffsetDateTime, display: &Display) -> String {
	relative_with(instant - time::OffsetDateTime::now_utc(), display)
}
/// Coarse "in …"/"… ago" phrasing with the same thresholds the web client's relative times use.
fn relative(delta: time::Duration) -> String {
	relative_with(delta, &Display::default())
}

fn relative_with(delta: time::Duration, display: &Display) -> String {
	let seconds = delta.whole_seconds();
	let ahead = seconds > 0;
	let seconds = seconds.unsigned_abs();
	// Rounded like the web client: 47 hours reads "2 days", not "1 day". A port may
	// round down instead, so 7.6 years reads "7 years".
	let round = |value: u64, unit: u64| {
		if display.floor_relative {
			value / unit
		} else {
			(value + unit / 2) / unit
		}
	};
	let minutes = round(seconds, 60);
	let hours = round(minutes, 60);
	let days = round(hours, 24);
	let months = round(days, 30);
	let years = round(days, 365);
	let amount = if seconds < 45 {
		"a few seconds".to_owned()
	} else if seconds < 90 {
		"a minute".to_owned()
	} else if minutes < 45 {
		format!("{minutes} minutes")
	} else if minutes < 90 {
		"an hour".to_owned()
	} else if hours < 22 {
		format!("{hours} hours")
	} else if hours < 36 {
		"a day".to_owned()
	} else if days < 26 {
		format!("{days} days")
	} else if days < 46 {
		"a month".to_owned()
	} else if days < 320 {
		format!("{months} months")
	} else if days < 548 {
		"a year".to_owned()
	} else {
		format!("{years} years")
	};
	if ahead {
		format!("in {amount}")
	} else {
		format!("{amount} ago")
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn formats_every_discord_timestamp_style() {
		let at = 1_700_000_000;
		for (style, expected) in [
			(b't', "22:13"),
			(b'T', "22:13:20"),
			(b'd', "11/14/2023"),
			(b'D', "November 14, 2023"),
			(b'f', "November 14, 2023 22:13"),
			(b'F', "Tuesday, November 14, 2023 22:13"),
		] {
			assert_eq!(
				super::discord_timestamp(at, style).as_deref(),
				Some(expected)
			);
		}
		assert_eq!(super::discord_timestamp(i64::MAX, b'f'), None);
	}
	#[test]
	fn phrases_relative_timestamps_in_both_directions() {
		let now = time::OffsetDateTime::now_utc().unix_timestamp();
		for (offset, expected) in [
			(-5, "a few seconds ago"),
			(-600, "10 minutes ago"),
			(-7_200, "2 hours ago"),
			(-864_000, "10 days ago"),
			(-15_552_000, "6 months ago"),
			(-157_680_000, "5 years ago"),
			(3_600, "in an hour"),
			(172_800, "in 2 days"),
		] {
			assert_eq!(
				super::discord_timestamp(now + offset, b'R').as_deref(),
				Some(expected),
				"offset {offset}"
			);
		}
	}
	#[test]
	fn a_port_can_choose_the_hour_format_and_shift_the_clock() {
		let instant = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
		let twelve = super::Display {
			hour: super::HourFormat::Twelve,
			..Default::default()
		};
		assert_eq!(super::clock(instant, &twelve), "10:13 PM");
		let shifted = super::Display {
			hour: super::HourFormat::TwentyFour,
			offset_minutes: 120,
			..Default::default()
		};
		assert_eq!(super::clock(instant, &shifted), "00:13");
		assert_eq!(
			super::clock(instant, &super::Display::default()),
			"22:13",
			"the default stays on the 24-hour clock"
		);
	}
	#[test]
	fn message_tooltips_show_a_local_date_and_the_selected_clock() {
		let instant = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
		let twelve = super::Display {
			hour: super::HourFormat::Twelve,
			..Default::default()
		};
		assert_eq!(
			super::message_tooltip(instant, &twelve),
			"November 14, 2023 · 10:13 PM"
		);
	}
	#[test]
	fn twelve_hour_clock_marks_midnight_noon_and_meridiem() {
		let display = super::Display {
			hour: super::HourFormat::Twelve,
			..Default::default()
		};
		for (seconds, expected) in [
			(0, "12:00 AM"),
			(3_600, "1:00 AM"),
			(43_200, "12:00 PM"),
			(46_800, "1:00 PM"),
			(86_340, "11:59 PM"),
		] {
			let instant = time::OffsetDateTime::from_unix_timestamp(seconds).unwrap();
			assert_eq!(super::clock(instant, &display), expected);
		}
	}
	#[test]
	fn display_offset_changes_the_message_date_and_time_together() {
		let instant = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
		let display = super::Display {
			hour: super::HourFormat::Twelve,
			offset_minutes: -300,
			..Default::default()
		};
		assert_eq!(super::clock(instant, &display), "7:00 PM");
		assert_eq!(
			super::message_tooltip(instant, &display),
			"December 31, 1969 · 7:00 PM"
		);
	}

	#[test]
	fn relative_phrases_round_down_when_a_port_asks() {
		// 7.6 years: the default rounds up to "8 years", a floor keeps "7 years".
		let delta = time::Duration::seconds(-(2_774 * 86_400));
		assert_eq!(
			super::relative_with(delta, &super::Display::default()),
			"8 years ago"
		);
		let floored = super::Display {
			floor_relative: true,
			..Default::default()
		};
		assert_eq!(super::relative_with(delta, &floored), "7 years ago");
		let more = time::Duration::seconds(-(3_504 * 86_400));
		assert_eq!(
			super::relative_with(more, &super::Display::default()),
			"10 years ago"
		);
		assert_eq!(super::relative_with(more, &floored), "9 years ago");
	}

	#[test]
	fn keeps_the_instant() {
		let utc = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
		assert_eq!(super::local(utc).unix_timestamp(), utc.unix_timestamp());
	}
}
