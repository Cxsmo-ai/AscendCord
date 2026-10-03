//! Import a Vencord / BetterDiscord CSS theme as a native theme package.
//!
//! Nothing here renders CSS. The stylesheet is read for Discord's custom properties on
//! the theme-wide selectors (`:root`, `html`, `body`, `.theme-dark`, `.theme-light`,
//! `.visual-refresh`), their values are resolved and mapped onto the native colour tokens,
//! and an optional background image is embedded. Everything else is ignored and reported.
//! Fetching `@import`ed files and images is left to the caller's `load` function, which
//! decides what may be read (local files beside the theme, and the network only when the
//! user allowed it).
use crate::{API_VERSION, Background, Error, ExtensionKind, Manifest, Package, Theme};
use std::collections::{BTreeMap, BTreeSet};

const MAX_TOTAL_CSS: usize = 4 * 1024 * 1024;
const MAX_IMPORTS: usize = 16;
const MAX_IMPORT_DEPTH: usize = 4;
const MAX_VAR_DEPTH: usize = 16;
const MAX_DECLARATIONS: usize = 50_000;
const MAX_WARNINGS: usize = 32;

/// A file the stylesheet refers to, already resolved against the sheet that named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
	/// A `/`-separated path inside the imported theme's folder (never `..` past it).
	Local(String),
	/// An absolute `https://` URL.
	Remote(String),
}

/// The converted package plus what the user should know before installing it.
#[derive(Debug)]
pub struct CssImport {
	pub package: Package,
	pub warnings: Vec<String>,
	/// Remote stylesheets and images that were not loaded (network not allowed or failed).
	pub skipped_remote: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
	Dark,
	Light,
}

/// Native token, then the custom properties that feed it, most specific first. Newer
/// "visual refresh" names come before the long-standing ones themes still set.
const ALIASES: &[(&str, &[&str])] = &[
	(
		"base",
		&[
			"--background-base-lowest",
			"--background-tertiary",
			"--background-primary",
		],
	),
	(
		"sidebar",
		&[
			"--background-base-lower",
			"--background-secondary",
			"--background-secondary-alt",
		],
	),
	("chat", &["--background-base-low", "--background-primary"]),
	(
		"raised",
		&[
			"--background-surface-high",
			"--background-surface-higher",
			"--background-floating",
		],
	),
	(
		"hover",
		&["--background-mod-subtle", "--background-modifier-hover"],
	),
	(
		"selected",
		&[
			"--background-mod-strong",
			"--background-mod-normal",
			"--background-modifier-selected",
			"--background-modifier-active",
		],
	),
	(
		"border",
		&[
			"--border-subtle",
			"--border-faint",
			"--background-modifier-accent",
		],
	),
	("text_strong", &["--text-strong", "--header-primary"]),
	("text", &["--text-default", "--text-normal"]),
	(
		"muted",
		&[
			"--text-muted",
			"--text-subtle",
			"--header-secondary",
			"--channels-default",
		],
	),
	("link", &["--text-link"]),
	(
		"accent",
		&["--brand-500", "--brand-experiment", "--text-brand"],
	),
	(
		"positive",
		&["--status-positive", "--green-360", "--status-green-560"],
	),
	(
		"warning",
		&["--status-warning", "--yellow-300", "--status-yellow-500"],
	),
	(
		"danger",
		&["--status-danger", "--red-400", "--status-red-500"],
	),
	(
		"mention_bg",
		&["--mention-background", "--background-mentioned"],
	),
	("mention_text", &["--mention-foreground"]),
];

/// Custom properties themes commonly use for a full-window background image.
const BACKGROUND_PROPERTIES: &[&str] = &[
	"--background-image",
	"--bg-image",
	"--app-background",
	"--app-bg",
	"--background-url",
	"--bg-url",
];

/// Convert `css` (from a file named `file_name`) into a theme package.
pub fn convert(
	css: &str,
	file_name: &str,
	load: &mut dyn FnMut(&Reference) -> Option<Vec<u8>>,
) -> Result<CssImport, Error> {
	let mut state = State {
		load,
		warnings: Vec::new(),
		skipped_remote: Vec::new(),
		imports: 0,
		total: 0,
		seen: BTreeSet::new(),
		shared: BTreeMap::new(),
		dark: BTreeMap::new(),
		light: BTreeMap::new(),
		images: Vec::new(),
		declarations: 0,
		base: Reference::Local(String::new()),
	};
	let meta = Meta::parse(css);
	state.sheet(css, 0)?;

	let mut theme = Theme::default();
	for (mode, palette) in [
		(Mode::Dark, &mut theme.dark),
		(Mode::Light, &mut theme.light),
	] {
		for (token, names) in ALIASES {
			let value = names.iter().find_map(|name| {
				let raw = state.lookup(mode, name)?;
				let resolved = state.resolve(mode, &raw, 0)?;
				match color(&resolved) {
					Ok(rgba) => Some(rgba),
					Err(note) => {
						state.warn(format!("{name}: {note}"));
						None
					}
				}
			});
			if let Some(rgba) = value {
				palette.colors.insert((*token).into(), hex(rgba));
			}
		}
		if let Some(accent) = palette
			.colors
			.get("accent")
			.and_then(|value| crate::parse_color(value).ok())
		{
			palette
				.colors
				.insert("accent_text".into(), hex(readable_on(accent)));
		}
	}
	if theme.dark.colors.is_empty() && theme.light.colors.is_empty() {
		state.warn("No Discord colour variables were found on theme-wide selectors.".into());
		return Err(Error::Invalid);
	}
	// A light-only or dark-only theme still applies in the other appearance.
	if theme.light.colors.is_empty() {
		theme.light.colors = theme.dark.colors.clone();
	} else if theme.dark.colors.is_empty() {
		theme.dark.colors = theme.light.colors.clone();
	}

	let background_image = state.background();
	if !background_image.is_empty() {
		for palette in [&mut theme.dark, &mut theme.light] {
			palette.background = Some(Background::default());
		}
	}

	let name = meta.name.clone().unwrap_or_else(|| display_name(file_name));
	let package = Package {
		background_image,
		cover_image: Vec::new(),
		manifest: Manifest {
			api_version: API_VERSION,
			id: package_id(&name, css),
			name,
			version: meta.version.unwrap_or_else(|| "1.0.0".into()),
			author: meta.author.unwrap_or_else(|| "Unknown".into()),
			license: "See the original theme".into(),
			source: String::new(),
			kind: ExtensionKind::Theme,
			capabilities: Vec::new(),
			actions: Vec::new(),
		},
		theme: Some(theme),
		wasm: Vec::new(),
	};
	package.validate()?;
	Ok(CssImport {
		package,
		warnings: state.warnings,
		skipped_remote: state.skipped_remote,
	})
}

struct State<'a> {
	load: &'a mut dyn FnMut(&Reference) -> Option<Vec<u8>>,
	warnings: Vec<String>,
	skipped_remote: Vec<String>,
	imports: usize,
	total: usize,
	seen: BTreeSet<String>,
	shared: BTreeMap<String, String>,
	dark: BTreeMap<String, String>,
	light: BTreeMap<String, String>,
	/// Candidate background images in source order, with the sheet they were declared in.
	images: Vec<(String, Reference)>,
	declarations: usize,
	/// The sheet being read: its folder (local) or its URL (remote).
	base: Reference,
}

impl State<'_> {
	fn warn(&mut self, note: String) {
		if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&note) {
			self.warnings.push(note);
		}
	}

	fn sheet(&mut self, css: &str, depth: usize) -> Result<(), Error> {
		self.total += css.len();
		if self.total > MAX_TOTAL_CSS {
			return Err(Error::Limit);
		}
		let css = strip_comments(css);
		let mut rest = css.as_str();
		loop {
			rest = rest.trim_start();
			if rest.is_empty() {
				return Ok(());
			}
			if let Some(after) = rest.strip_prefix("@import") {
				let end = statement_end(after);
				self.import(after[..end].trim(), depth)?;
				rest = after.get(end + 1..).unwrap_or("");
				continue;
			}
			let Some(open) = find_outside(rest, b'{') else {
				return Ok(());
			};
			let prelude = rest[..open].trim();
			// A statement at-rule (`@charset …;`) before the block: skip it.
			if let Some(semi) = find_outside(prelude, b';') {
				rest = &rest[semi + 1..];
				continue;
			}
			let close = matching_brace(rest, open).ok_or(Error::Invalid)?;
			let body = &rest[open + 1..close];
			rest = &rest[close + 1..];
			if let Some(at) = prelude.strip_prefix('@') {
				// Conditional groups hold ordinary rules; the rest (keyframes, fonts) is skipped.
				let keyword = at
					.split(|c: char| !c.is_ascii_alphabetic() && c != '-')
					.next();
				if matches!(keyword, Some("media" | "supports" | "layer" | "container")) {
					self.sheet(body, depth)?;
					self.total -= body.len();
				}
				continue;
			}
			self.rule(prelude, body);
		}
	}

	fn import(&mut self, statement: &str, depth: usize) -> Result<(), Error> {
		let Some(target) = import_target(statement) else {
			self.warn("An @import without a file was skipped.".into());
			return Ok(());
		};
		if depth >= MAX_IMPORT_DEPTH || self.imports >= MAX_IMPORTS {
			self.warn(format!("Too many nested imports; skipped {target}"));
			return Ok(());
		}
		let Some(reference) = resolve(&self.base, &target) else {
			self.warn(format!("Unsupported import location: {target}"));
			return Ok(());
		};
		let key = match &reference {
			Reference::Local(path) => format!("local:{path}"),
			Reference::Remote(url) => format!("remote:{url}"),
		};
		if !self.seen.insert(key) {
			return Ok(());
		}
		self.imports += 1;
		match (self.load)(&reference) {
			Some(bytes) => match String::from_utf8(bytes) {
				Ok(text) => {
					// Relative references inside the imported sheet start from its own location.
					let child = match &reference {
						Reference::Local(path) => Reference::Local(parent(path).to_owned()),
						Reference::Remote(url) => Reference::Remote(url.clone()),
					};
					let outer = std::mem::replace(&mut self.base, child);
					let result = self.sheet(&text, depth + 1);
					self.base = outer;
					result?;
				}
				Err(_) => self.warn(format!("{target} is not UTF-8 text")),
			},
			None => match reference {
				Reference::Remote(url) => self.skipped_remote.push(url),
				Reference::Local(path) => self.warn(format!("Could not read {path}")),
			},
		}
		Ok(())
	}

	fn rule(&mut self, selectors: &str, body: &str) {
		let mut modes = Vec::new();
		for selector in selectors.split(',') {
			match theme_selector(selector) {
				Some(mode) => modes.push(mode),
				None => {
					if body.contains("--") {
						let selector = selector.trim();
						self.warn(format!(
							"Variables under `{}` apply to part of Discord only and were skipped.",
							selector.chars().take(60).collect::<String>()
						));
					}
				}
			}
		}
		if modes.is_empty() {
			return;
		}
		for declaration in split_outside(body, b';') {
			let Some(colon) = declaration.find(':') else {
				continue;
			};
			let name = declaration[..colon].trim();
			let mut value = declaration[colon + 1..].trim();
			if let Some(stripped) = value.strip_suffix("!important") {
				value = stripped.trim_end();
			}
			self.declarations += 1;
			if self.declarations > MAX_DECLARATIONS {
				return;
			}
			if !name.starts_with("--") {
				// Plain background images on the page root are a common way to set a wallpaper.
				if matches!(name, "background-image" | "background") && value.contains("url(") {
					self.images.push((value.to_owned(), self.base.clone()));
				}
				continue;
			}
			if BACKGROUND_PROPERTIES.contains(&name) {
				self.images.push((value.to_owned(), self.base.clone()));
			}
			for mode in &modes {
				let map = match mode {
					Scope::Shared => &mut self.shared,
					Scope::Dark => &mut self.dark,
					Scope::Light => &mut self.light,
				};
				map.insert(name.to_owned(), value.to_owned());
			}
		}
	}

	/// A mode-specific declaration wins over a shared one, as `.theme-dark` outranks `:root`.
	fn lookup(&self, mode: Mode, name: &str) -> Option<String> {
		let specific = match mode {
			Mode::Dark => &self.dark,
			Mode::Light => &self.light,
		};
		specific
			.get(name)
			.or_else(|| self.shared.get(name))
			.cloned()
	}

	fn resolve(&self, mode: Mode, value: &str, depth: usize) -> Option<String> {
		if depth > MAX_VAR_DEPTH {
			return None;
		}
		let Some(start) = value.find("var(") else {
			return Some(value.trim().to_owned());
		};
		let open = start + 3;
		let close = matching_paren(value, open)?;
		let inner = &value[open + 1..close];
		let (name, fallback) = match find_outside(inner, b',') {
			Some(comma) => (inner[..comma].trim(), Some(inner[comma + 1..].trim())),
			None => (inner.trim(), None),
		};
		let replacement = match self.lookup(mode, name) {
			Some(found) => self.resolve(mode, &found, depth + 1)?,
			None => self.resolve(mode, fallback?, depth + 1)?,
		};
		let rebuilt = format!("{}{}{}", &value[..start], replacement, &value[close + 1..]);
		self.resolve(mode, &rebuilt, depth + 1)
	}

	/// The first background image that can be read; others are ignored.
	fn background(&mut self) -> Vec<u8> {
		let candidates = std::mem::take(&mut self.images);
		for (raw, base) in candidates {
			let value = self.resolve(Mode::Dark, &raw, 0).unwrap_or(raw);
			let Some(url) = first_url(&value) else {
				continue;
			};
			if let Some(data) = url.strip_prefix("data:") {
				match data_image(data) {
					Some(bytes) if bytes.len() <= crate::MAX_BACKGROUND_BYTES => return bytes,
					_ => self.warn(
						"The theme's inline background image is unsupported or too large.".into(),
					),
				}
				continue;
			}
			let Some(reference) = resolve(&base, &url) else {
				continue;
			};
			match (self.load)(&reference) {
				Some(bytes)
					if bytes.len() <= crate::MAX_BACKGROUND_BYTES && image_signature(&bytes) =>
				{
					return bytes;
				}
				Some(_) => self.warn(
					"The theme's background image is not a PNG, JPEG or WebP under 2 MiB.".into(),
				),
				None => {
					if let Reference::Remote(url) = reference {
						self.skipped_remote.push(url);
					}
				}
			}
		}
		Vec::new()
	}
}

#[derive(Clone, Copy)]
enum Scope {
	Shared,
	Dark,
	Light,
}

/// Selectors that address the whole client. Anything else targets a component.
fn theme_selector(selector: &str) -> Option<Scope> {
	let selector = selector.trim();
	if selector.is_empty() {
		return None;
	}
	let mut scope = Scope::Shared;
	let mut rest = selector;
	while !rest.is_empty() {
		rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '>');
		let token_end = if rest.starts_with(":where(") || rest.starts_with(":is(") {
			matching_paren(rest, rest.find('(')?)? + 1
		} else {
			rest[1.min(rest.len())..]
				.find(['.', ':', ' ', '>', '[', '#'])
				.map_or(rest.len(), |i| i + 1)
		};
		let token = &rest[..token_end];
		match token {
			":root"
			| "html"
			| "body"
			| "*"
			| ".visual-refresh"
			| ".theme-dark"
			| ".theme-darker"
			| ".theme-midnight"
			| ".theme-light"
			| ":where(.theme-dark)" => {}
			_ if token.starts_with(":where(") || token.starts_with(":is(") => {
				let inner = &token[token.find('(').unwrap_or(0) + 1..];
				if !inner.contains("theme-") && !inner.contains("visual-refresh") {
					return None;
				}
			}
			"" => break,
			_ => return None,
		}
		if token.contains("theme-light") {
			scope = Scope::Light;
		} else if token.contains("theme-dark") || token.contains("theme-midnight") {
			scope = Scope::Dark;
		}
		rest = &rest[token_end..];
	}
	Some(scope)
}

/// Parse a resolved CSS colour into RGBA.
fn color(value: &str) -> Result<[u8; 4], String> {
	let value = value.trim();
	let lower = value.to_ascii_lowercase();
	if let Some(hex) = lower.strip_prefix('#') {
		return hex_color(hex).ok_or_else(|| format!("unsupported colour {value}"));
	}
	match lower.as_str() {
		"white" => return Ok([255, 255, 255, 255]),
		"black" => return Ok([0, 0, 0, 255]),
		"transparent" => return Ok([0, 0, 0, 0]),
		_ => {}
	}
	let open = lower
		.find('(')
		.ok_or_else(|| format!("unsupported colour {value}"))?;
	let function = lower[..open].trim();
	let close = matching_paren(&lower, open).ok_or_else(|| format!("unbalanced {value}"))?;
	let inner = &lower[open + 1..close];
	match function {
		"rgb" | "rgba" => rgb_function(inner),
		"hsl" | "hsla" => hsl_function(inner),
		"color-mix" => color_mix(inner),
		_ => Err(format!("{function}() colours are not supported")),
	}
}

fn hex_color(hex: &str) -> Option<[u8; 4]> {
	if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
		return None;
	}
	let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
	let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
	match hex.len() {
		3 | 4 => {
			let mut out = [255; 4];
			for (i, slot) in out.iter_mut().enumerate().take(hex.len()) {
				*slot = digit(i)? * 17;
			}
			Some(out)
		}
		6 | 8 => {
			let mut out = [255; 4];
			for (i, slot) in out.iter_mut().enumerate().take(hex.len() / 2) {
				*slot = pair(i * 2)?;
			}
			Some(out)
		}
		_ => None,
	}
}

/// Components of `rgb()`/`hsl()`: commas or spaces, with an optional `/ alpha`.
fn components(inner: &str) -> (Vec<&str>, Option<&str>) {
	let (main, alpha) = match find_outside(inner, b'/') {
		Some(slash) => (&inner[..slash], Some(inner[slash + 1..].trim())),
		None => (inner, None),
	};
	let parts: Vec<&str> = if find_outside(main, b',').is_some() {
		split_outside(main, b',')
			.into_iter()
			.map(str::trim)
			.filter(|part| !part.is_empty())
			.collect()
	} else {
		split_whitespace_outside(main)
	};
	if alpha.is_none() && parts.len() == 4 {
		return (parts[..3].to_vec(), Some(parts[3]));
	}
	(parts, alpha)
}

fn number(part: &str, percent_scale: f32) -> Option<f32> {
	match part.strip_suffix('%') {
		Some(percent) => percent
			.trim()
			.parse::<f32>()
			.ok()
			.map(|v| v / 100.0 * percent_scale),
		None => part.trim().parse::<f32>().ok(),
	}
}

fn alpha(part: Option<&str>) -> Result<u8, String> {
	match part {
		None => Ok(255),
		Some(part) => number(part, 1.0)
			.map(|a| (a.clamp(0.0, 1.0) * 255.0).round() as u8)
			.ok_or_else(|| format!("unsupported alpha {part}")),
	}
}

fn rgb_function(inner: &str) -> Result<[u8; 4], String> {
	let (parts, a) = components(inner);
	if parts.len() != 3 {
		return Err(format!("unsupported rgb({inner})"));
	}
	let mut out = [0, 0, 0, alpha(a)?];
	for (slot, part) in out.iter_mut().zip(&parts) {
		*slot = number(part, 255.0)
			.ok_or_else(|| format!("unsupported rgb({inner})"))?
			.clamp(0.0, 255.0)
			.round() as u8;
	}
	Ok(out)
}

fn hsl_function(inner: &str) -> Result<[u8; 4], String> {
	let (parts, a) = components(inner);
	let error = || format!("unsupported hsl({inner})");
	if parts.len() != 3 {
		return Err(error());
	}
	let hue = parts[0]
		.trim_end_matches("deg")
		.parse::<f32>()
		.map_err(|_| error())?;
	// Discord multiplies saturation by a user setting: `calc(var(--saturation-factor, 1) * 50%)`.
	let saturation = percent_or_calc(parts[1]).ok_or_else(error)?;
	let lightness = percent_or_calc(parts[2]).ok_or_else(error)?;
	let [r, g, b] = hsl_to_rgb(hue, saturation, lightness);
	Ok([r, g, b, alpha(a)?])
}

/// `50%`, or Discord's `calc(1*50%)` form after variables resolved.
fn percent_or_calc(part: &str) -> Option<f32> {
	let part = part.trim();
	if let Some(inner) = part.strip_prefix("calc(").and_then(|p| p.strip_suffix(')')) {
		let (factor, percent) = inner.split_once('*')?;
		return Some(factor.trim().parse::<f32>().ok()? * number(percent.trim(), 1.0)?);
	}
	number(part, 1.0).filter(|_| part.ends_with('%'))
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> [u8; 3] {
	let s = saturation.clamp(0.0, 1.0);
	let l = lightness.clamp(0.0, 1.0);
	let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
	let h = hue.rem_euclid(360.0) / 60.0;
	let x = c * (1.0 - (h % 2.0 - 1.0).abs());
	let (r, g, b) = match h as u32 {
		0 => (c, x, 0.0),
		1 => (x, c, 0.0),
		2 => (0.0, c, x),
		3 => (0.0, x, c),
		4 => (x, 0.0, c),
		_ => (c, 0.0, x),
	};
	let m = l - c / 2.0;
	[r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// `color-mix(in srgb, a p%, b q%)`. Other colour spaces would need their own maths, so
/// they are reported instead of approximated.
fn color_mix(inner: &str) -> Result<[u8; 4], String> {
	let parts = split_outside(inner, b',');
	if parts.len() != 3 || parts[0].trim() != "in srgb" {
		return Err("color-mix() is supported only `in srgb`".into());
	}
	let side = |part: &str| -> Result<([u8; 4], Option<f32>), String> {
		let part = part.trim();
		match part.rsplit_once(' ').filter(|(_, p)| p.ends_with('%')) {
			Some((colour, percent)) => Ok((color(colour)?, number(percent, 1.0))),
			None => Ok((color(part)?, None)),
		}
	};
	let (a, pa) = side(parts[1])?;
	let (b, pb) = side(parts[2])?;
	let weight = match (pa, pb) {
		(Some(p), _) => p,
		(None, Some(q)) => 1.0 - q,
		(None, None) => 0.5,
	}
	.clamp(0.0, 1.0);
	let mut out = [0; 4];
	for i in 0..4 {
		out[i] = (f32::from(a[i]) * weight + f32::from(b[i]) * (1.0 - weight)).round() as u8;
	}
	Ok(out)
}

fn hex([r, g, b, a]: [u8; 4]) -> String {
	if a == 255 {
		format!("#{r:02X}{g:02X}{b:02X}")
	} else {
		format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
	}
}

/// Black or white, whichever reads better on `background` (WCAG relative luminance).
fn readable_on([r, g, b, _]: [u8; 4]) -> [u8; 4] {
	let channel = |v: u8| {
		let v = f32::from(v) / 255.0;
		if v <= 0.040_45 {
			v / 12.92
		} else {
			((v + 0.055) / 1.055).powf(2.4)
		}
	};
	let luminance = 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
	if luminance > 0.179 {
		[0x10, 0x12, 0x14, 255]
	} else {
		[255, 255, 255, 255]
	}
}

#[derive(Default)]
struct Meta {
	name: Option<String>,
	author: Option<String>,
	version: Option<String>,
}

impl Meta {
	/// BetterDiscord-style `@name`/`@author`/`@version` lines in the first comment block.
	/// Display text only: length-limited and free of control characters.
	fn parse(css: &str) -> Self {
		let mut meta = Self::default();
		let Some(start) = css.find("/*") else {
			return meta;
		};
		let Some(end) = css[start..].find("*/") else {
			return meta;
		};
		for line in css[start + 2..start + end].lines() {
			let line = line.trim().trim_start_matches('*').trim();
			let Some((key, value)) = line
				.strip_prefix('@')
				.and_then(|l| l.split_once(char::is_whitespace))
			else {
				continue;
			};
			let value: String = value
				.trim()
				.chars()
				.filter(|c| !c.is_control())
				.take(64)
				.collect();
			if value.is_empty() {
				continue;
			}
			match key {
				"name" => meta.name = Some(value),
				"author" => meta.author = Some(value),
				"version" => meta.version = Some(value),
				_ => {}
			}
		}
		meta
	}
}

fn display_name(file_name: &str) -> String {
	let stem = file_name
		.rsplit(['/', '\\'])
		.next()
		.unwrap_or(file_name)
		.trim_end_matches(".css")
		.trim_end_matches(".theme");
	let name: String = stem.chars().filter(|c| !c.is_control()).take(64).collect();
	if name.trim().is_empty() {
		"Imported theme".into()
	} else {
		name
	}
}

/// `css-<slug>-<hash>`: stable for the same file, distinct for different files.
fn package_id(name: &str, css: &str) -> String {
	let mut slug = String::new();
	for c in name.chars().flat_map(char::to_lowercase) {
		if c.is_ascii_alphanumeric() {
			slug.push(c);
		} else if !slug.ends_with('-') && !slug.is_empty() {
			slug.push('-');
		}
		if slug.len() >= 40 {
			break;
		}
	}
	let slug = slug.trim_end_matches('-');
	// FNV-1a: only needs to separate different files, not resist attackers.
	let mut hash: u32 = 0x811c_9dc5;
	for byte in css.bytes() {
		hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
	}
	if slug.is_empty() {
		format!("css-theme-{hash:08x}")
	} else {
		format!("css-{slug}-{hash:08x}")
	}
}

/// Resolve `target` as written in a sheet located at `base`. Remote sheets resolve like a
/// browser would; local paths stay inside the theme's folder. Other schemes are refused.
fn resolve(base: &Reference, target: &str) -> Option<Reference> {
	let target = target.trim();
	let lower = target.to_ascii_lowercase();
	if lower.starts_with("https://") {
		return Some(Reference::Remote(target.to_owned()));
	}
	if lower.contains("://") || lower.starts_with("//") || lower.starts_with("data:") {
		return None;
	}
	match base {
		Reference::Remote(url) => {
			let joined = url::Url::parse(url).ok()?.join(target).ok()?;
			(joined.scheme() == "https").then(|| Reference::Remote(joined.to_string()))
		}
		Reference::Local(folder) => {
			if target.starts_with(['/', '\\']) || target.contains(':') {
				return None;
			}
			let mut parts: Vec<&str> = folder.split('/').filter(|p| !p.is_empty()).collect();
			for part in target.split(['/', '\\']) {
				match part {
					"" | "." => {}
					".." => {
						parts.pop()?;
					}
					part => parts.push(part),
				}
			}
			(!parts.is_empty()).then(|| Reference::Local(parts.join("/")))
		}
	}
}

/// The folder part of a local reference path.
fn parent(path: &str) -> &str {
	path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// The file named by an `@import` statement body.
fn import_target(statement: &str) -> Option<String> {
	let statement = statement.trim();
	if let Some(url) = first_url(statement) {
		return Some(url);
	}
	let quote = statement
		.chars()
		.next()
		.filter(|c| *c == '"' || *c == '\'')?;
	let rest = &statement[1..];
	Some(rest[..rest.find(quote)?].to_owned())
}

fn first_url(value: &str) -> Option<String> {
	let start = value.find("url(")?;
	let close = matching_paren(value, start + 3)?;
	let inner = value[start + 4..close].trim();
	let inner = inner
		.strip_prefix('"')
		.and_then(|v| v.strip_suffix('"'))
		.or_else(|| inner.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
		.unwrap_or(inner);
	(!inner.is_empty()).then(|| inner.to_owned())
}

/// `image/png;base64,...` etc.; only raster formats the background loader accepts.
fn data_image(data: &str) -> Option<Vec<u8>> {
	let (header, body) = data.split_once(',')?;
	let header = header.to_ascii_lowercase();
	if !header.ends_with(";base64")
		|| !["image/png", "image/jpeg", "image/jpg", "image/webp"]
			.iter()
			.any(|kind| header.starts_with(kind))
	{
		return None;
	}
	let bytes = base64(body)?;
	image_signature(&bytes).then_some(bytes)
}

fn image_signature(bytes: &[u8]) -> bool {
	bytes.starts_with(b"\x89PNG\r\n\x1a\n")
		|| bytes.starts_with(&[0xFF, 0xD8, 0xFF])
		|| (bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

fn base64(text: &str) -> Option<Vec<u8>> {
	let mut out = Vec::with_capacity(text.len() * 3 / 4);
	let mut buffer = 0u32;
	let mut bits = 0;
	for byte in text.bytes() {
		let value = match byte {
			b'A'..=b'Z' => byte - b'A',
			b'a'..=b'z' => byte - b'a' + 26,
			b'0'..=b'9' => byte - b'0' + 52,
			b'+' | b'-' => 62,
			b'/' | b'_' => 63,
			b'=' => break,
			b if b.is_ascii_whitespace() => continue,
			_ => return None,
		};
		buffer = (buffer << 6) | u32::from(value);
		bits += 6;
		if bits >= 8 {
			bits -= 8;
			out.push((buffer >> bits) as u8);
		}
		if out.len() > crate::MAX_BACKGROUND_BYTES {
			return None;
		}
	}
	Some(out)
}

/// Remove `/* … */` comments, leaving quoted strings (which may contain `/*`) intact.
fn strip_comments(css: &str) -> String {
	let mut out = String::with_capacity(css.len());
	let mut chars = css.chars().peekable();
	let mut quote = None;
	while let Some(c) = chars.next() {
		match quote {
			Some(q) => {
				out.push(c);
				if c == '\\' {
					if let Some(escaped) = chars.next() {
						out.push(escaped);
					}
				} else if c == q {
					quote = None;
				}
			}
			None if c == '"' || c == '\'' => {
				quote = Some(c);
				out.push(c);
			}
			None if c == '/' && chars.peek() == Some(&'*') => {
				chars.next();
				let mut previous = '\0';
				for inner in chars.by_ref() {
					if previous == '*' && inner == '/' {
						break;
					}
					previous = inner;
				}
			}
			None => out.push(c),
		}
	}
	out
}

/// Byte offset of `needle` outside strings and brackets.
fn find_outside(text: &str, needle: u8) -> Option<usize> {
	let mut depth = 0i32;
	let mut quote = None;
	for (i, byte) in text.bytes().enumerate() {
		match quote {
			Some(q) if byte == q => quote = None,
			Some(_) => {}
			None => match byte {
				b'"' | b'\'' => quote = Some(byte),
				b'(' | b'[' => depth += 1,
				b')' | b']' => depth -= 1,
				_ if byte == needle && depth <= 0 => return Some(i),
				_ => {}
			},
		}
	}
	None
}

fn split_outside(text: &str, separator: u8) -> Vec<&str> {
	let mut parts = Vec::new();
	let mut rest = text;
	while let Some(i) = find_outside(rest, separator) {
		parts.push(&rest[..i]);
		rest = &rest[i + 1..];
	}
	if !rest.trim().is_empty() {
		parts.push(rest);
	}
	parts
}

/// Whitespace-separated parts, keeping `calc(1 * 50%)` and similar together.
fn split_whitespace_outside(text: &str) -> Vec<&str> {
	let mut parts = Vec::new();
	let mut depth = 0i32;
	let mut start = None;
	for (i, byte) in text.bytes().enumerate() {
		match byte {
			b'(' => depth += 1,
			b')' => depth -= 1,
			_ => {}
		}
		if byte.is_ascii_whitespace() && depth <= 0 {
			if let Some(begin) = start.take() {
				parts.push(&text[begin..i]);
			}
		} else if start.is_none() {
			start = Some(i);
		}
	}
	if let Some(begin) = start {
		parts.push(&text[begin..]);
	}
	parts
}

fn statement_end(text: &str) -> usize {
	find_outside(text, b';').unwrap_or(text.len())
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
	matching(text, open, b'{', b'}')
}

fn matching_paren(text: &str, open: usize) -> Option<usize> {
	matching(text, open, b'(', b')')
}

fn matching(text: &str, open: usize, left: u8, right: u8) -> Option<usize> {
	let mut depth = 0;
	let mut quote = None;
	for (i, byte) in text.bytes().enumerate().skip(open) {
		match quote {
			Some(q) if byte == q => quote = None,
			Some(_) => {}
			None if byte == b'"' || byte == b'\'' => quote = Some(byte),
			None if byte == left => depth += 1,
			None if byte == right => {
				depth -= 1;
				if depth == 0 {
					return Some(i);
				}
			}
			None => {}
		}
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	fn offline(_: &Reference) -> Option<Vec<u8>> {
		None
	}

	fn colors(import: &CssImport, dark: bool) -> &BTreeMap<String, String> {
		let theme = import.package.theme.as_ref().unwrap();
		if dark {
			&theme.dark.colors
		} else {
			&theme.light.colors
		}
	}

	#[test]
	fn legacy_and_refresh_variables_map_with_mode_precedence() {
		let css = "/**\n * @name Night Owl\n * @author someone\n * @version 2.1.0\n */\n\
			:root { --brand-experiment: #5865f2; --text-normal: rgb(220, 221, 222); }\n\
			.theme-dark { --background-primary: #36393f; --background-secondary: hsl(220 7.7% 22.9%); }\n\
			.theme-light { --background-primary: #ffffff; --text-normal: #2e3338; }\n\
			.visual-refresh.theme-dark { --background-base-lower: #121214; }";
		let import = convert(css, "night.theme.css", &mut offline).unwrap();
		let manifest = &import.package.manifest;
		assert_eq!(manifest.name, "Night Owl");
		assert_eq!(manifest.author, "someone");
		assert_eq!(manifest.version, "2.1.0");
		assert!(manifest.id.starts_with("css-night-owl-"));
		let dark = colors(&import, true);
		assert_eq!(dark["chat"], "#36393F");
		assert_eq!(
			dark["sidebar"], "#121214",
			"refresh name wins over the legacy one"
		);
		assert_eq!(dark["accent"], "#5865F2");
		assert_eq!(dark["accent_text"], "#FFFFFF");
		assert_eq!(dark["text"], "#DCDDDE");
		let light = colors(&import, false);
		assert_eq!(light["chat"], "#FFFFFF");
		assert_eq!(
			light["text"], "#2E3338",
			"light-only value beats the shared one"
		);
	}

	#[test]
	fn variables_resolve_through_fallbacks_and_discords_saturation_calc() {
		let css = ":root { --saturation-factor: 1; --my-bg: #102030;\
			--background-primary: var(--my-bg);\
			--background-secondary: var(--missing, rgba(16 32 48 / 50%));\
			--text-normal: hsl(0, calc(var(--saturation-factor, 1) * 0%), 100%);\
			--brand-experiment: color-mix(in srgb, #000000 50%, #ffffff); }";
		let import = convert(css, "a.css", &mut offline).unwrap();
		let dark = colors(&import, true);
		assert_eq!(dark["chat"], "#102030");
		assert_eq!(dark["sidebar"], "#10203080");
		assert_eq!(dark["text"], "#FFFFFF");
		assert_eq!(dark["accent"], "#808080");
		assert_eq!(
			dark,
			colors(&import, false),
			"shared values apply to both appearances"
		);
	}

	#[test]
	fn component_selectors_and_unsupported_spaces_are_reported_not_applied() {
		let css = ":root { --background-primary: #111111; --text-normal: color-mix(in oklab, red, blue); }\
			.chat_abc .message { --background-primary: #ff0000; }";
		let import = convert(css, "b.css", &mut offline).unwrap();
		assert_eq!(colors(&import, true)["chat"], "#111111");
		assert!(!colors(&import, true).contains_key("text"));
		assert!(import.warnings.iter().any(|w| w.contains("only `in srgb`")));
		assert!(
			import
				.warnings
				.iter()
				.any(|w| w.contains("part of Discord only"))
		);
	}

	#[test]
	fn imports_go_through_the_loader_and_remote_ones_are_listed_when_not_loaded() {
		let css = "@import url('https://example.com/base.css');\n@import \"local.css\";\n\
			@import url('file:///etc/passwd');\n:root { --brand-experiment: #00ff00; }";
		let mut asked = Vec::new();
		let import = convert(css, "c.css", &mut |reference: &Reference| {
			asked.push(reference.clone());
			match reference {
				Reference::Local(path) if path == "local.css" => {
					Some(b":root{--background-primary:#222222}".to_vec())
				}
				_ => None,
			}
		})
		.unwrap();
		assert_eq!(
			asked,
			[
				Reference::Remote("https://example.com/base.css".into()),
				Reference::Local("local.css".into())
			],
			"file: URLs never reach the loader"
		);
		assert_eq!(import.skipped_remote, ["https://example.com/base.css"]);
		assert_eq!(colors(&import, true)["chat"], "#222222");
	}

	#[test]
	fn imports_resolve_against_the_sheet_that_names_them() {
		let mut asked = Vec::new();
		let import = convert(
			"@import 'sub/a.css'; @import 'other/a.css'; :root { --brand-experiment: #010203; }",
			"root.css",
			&mut |reference: &Reference| {
				asked.push(reference.clone());
				let sheet: &[u8] = match reference {
					Reference::Local(path) if path == "sub/a.css" => {
						b"@import 'b.css'; @import '../../escape.css';"
					}
					Reference::Local(path) if path == "sub/b.css" => {
						b":root { --background-primary: #222222; }"
					}
					Reference::Local(path) if path == "other/a.css" => {
						b"@import url(https://example.com/themes/main.css);"
					}
					Reference::Remote(url) if url == "https://example.com/themes/main.css" => {
						b"@import './base.css';"
					}
					_ => return None,
				};
				Some(sheet.to_vec())
			},
		)
		.unwrap();
		assert_eq!(
			asked,
			[
				Reference::Local("sub/a.css".into()),
				Reference::Local("sub/b.css".into()),
				Reference::Local("other/a.css".into()),
				Reference::Remote("https://example.com/themes/main.css".into()),
				Reference::Remote("https://example.com/themes/base.css".into()),
			],
			"`../../escape.css` leaves the theme folder and is never requested"
		);
		assert_eq!(colors(&import, true)["chat"], "#222222");
	}

	#[test]
	fn comments_inside_strings_are_kept() {
		assert_eq!(
			strip_comments("a { --x: \"a/*not*/b\"; } /* gone */ b"),
			"a { --x: \"a/*not*/b\"; }  b"
		);
	}

	#[test]
	fn import_cycles_and_depth_are_bounded() {
		let mut calls = 0;
		let import = convert(
			"@import 'a.css'; :root { --brand-experiment: #123456; }",
			"root.css",
			&mut |_: &Reference| {
				calls += 1;
				Some(b"@import 'a.css'; @import 'b.css';".to_vec())
			},
		)
		.unwrap();
		assert!(calls <= MAX_IMPORTS);
		assert_eq!(colors(&import, true)["accent"], "#123456");
	}

	#[test]
	fn inline_background_images_are_embedded() {
		let png = [
			0x89u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 0,
		];
		let encoded = "iVBORw0KGgoAAAAA";
		assert_eq!(base64(encoded).unwrap(), png);
		let css = format!(
			":root {{ --background-image: url(\"data:image/png;base64,{encoded}\"); --background-primary: #000; }}"
		);
		let import = convert(&css, "d.css", &mut offline).unwrap();
		assert_eq!(import.package.background_image, png);
		assert!(
			import
				.package
				.theme
				.as_ref()
				.unwrap()
				.dark
				.background
				.is_some()
		);
	}

	#[test]
	fn a_sheet_without_theme_colours_is_rejected() {
		assert!(convert(".foo { color: red; }", "e.css", &mut offline).is_err());
	}

	#[test]
	fn colour_syntax() {
		assert_eq!(color("#abc").unwrap(), [0xAA, 0xBB, 0xCC, 255]);
		assert_eq!(color("#11223344").unwrap(), [0x11, 0x22, 0x33, 0x44]);
		assert_eq!(color("rgba(255, 0, 0, 0.5)").unwrap(), [255, 0, 0, 128]);
		assert_eq!(color("rgb(0% 100% 0%)").unwrap(), [0, 255, 0, 255]);
		assert_eq!(color("hsl(240deg 100% 50%)").unwrap(), [0, 0, 255, 255]);
		assert!(color("lab(50% 0 0)").is_err());
	}
}
