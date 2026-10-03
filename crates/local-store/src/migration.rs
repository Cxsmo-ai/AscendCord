//! One-time copy of a pre-rename (tesktop2) data folder into the AscendCord folder.
//!
//! The legacy folder is only ever read: nothing in it is moved, renamed or deleted, so an
//! older build keeps working and a failed run can simply be retried. Only durable user
//! state is copied — the SQLite store and installed extensions. Caches, crash dumps and the
//! developer voice-bridge control files (which would rejoin a call) stay behind.
use rusqlite::{Connection, OpenFlags};
use std::{
	fs::{self, OpenOptions},
	io,
	path::Path,
};

/// Folder name before the AscendCord rename, under the same base directory.
pub const LEGACY_DIR: &str = "tesktop2";
const MARKER: &str = ".ascendcord-migration-v1";
const LOCK: &str = ".migration.lock";
const DATABASE: &str = "client.sqlite3";
const STAGING: &str = ".client.sqlite3.migrating";

#[derive(Debug)]
pub enum MigrationError {
	Io(io::Error),
	Sql(rusqlite::Error),
	/// `PRAGMA quick_check` rejected the copied database; the legacy one is untouched.
	Corrupt(String),
	/// SQLite takes the destination as text; a non-UTF-8 path cannot be named.
	Path,
}
impl From<io::Error> for MigrationError {
	fn from(value: io::Error) -> Self {
		Self::Io(value)
	}
}
impl From<rusqlite::Error> for MigrationError {
	fn from(value: rusqlite::Error) -> Self {
		Self::Sql(value)
	}
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
	pub database: bool,
	pub extension_files: usize,
	pub plugin_settings: bool,
}

/// Copy durable legacy data from `old` into `new` once. Safe to call on every start: it
/// returns at once after a completed run, and concurrent processes take turns on a lock.
pub fn migrate(old: &Path, new: &Path) -> Result<Report, MigrationError> {
	fs::create_dir_all(new)?;
	let lock = OpenOptions::new()
		.create(true)
		.truncate(false)
		.read(true)
		.write(true)
		.open(new.join(LOCK))?;
	lock.lock()?;
	let result = migrate_locked(old, new);
	let _ = lock.unlock();
	result
}

/// Whether `new` may be used after a migration attempt. A failure that left a legacy store
/// uncopied keeps the folder unusable for this run, so no fresh store is created in its place
/// and the next start retries the copy.
pub fn usable(result: &Result<Report, MigrationError>, old: &Path, new: &Path) -> bool {
	result.is_ok() || !old.join(DATABASE).is_file() || new.join(DATABASE).is_file()
}

fn migrate_locked(old: &Path, new: &Path) -> Result<Report, MigrationError> {
	let marker = new.join(MARKER);
	if marker.is_file() {
		return Ok(Report::default());
	}
	let mut report = Report::default();
	if old.is_dir() {
		let database = new.join(DATABASE);
		if old.join(DATABASE).is_file() && !database.exists() {
			copy_database(&old.join(DATABASE), new)?;
			report.database = true;
		}
		report.extension_files = copy_missing(&old.join("extensions"), &new.join("extensions"))?;
		// TestCord plugin settings, renamed with the plugin crate.
		let plugins = new.join("testcord-plugins.json");
		if old.join("tesktop-plugins.json").is_file() && !plugins.exists() {
			let staging = new.join(".testcord-plugins.json.migrating");
			fs::copy(old.join("tesktop-plugins.json"), &staging)?;
			fs::rename(&staging, plugins)?;
			report.plugin_settings = true;
		}
	}
	// Last, so an interrupted run is retried rather than silently skipped.
	fs::write(marker, b"")?;
	Ok(report)
}

/// `VACUUM INTO` reads through SQLite, so commits still in the legacy WAL are included,
/// and the result is a compact standalone file. It is checked before it takes its name.
fn copy_database(source: &Path, new: &Path) -> Result<(), MigrationError> {
	let staging = new.join(STAGING);
	remove_if_present(&staging)?;
	let target = staging.to_str().ok_or(MigrationError::Path)?;
	{
		let legacy = Connection::open_with_flags(
			source,
			OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
		)?;
		legacy.busy_timeout(std::time::Duration::from_secs(5))?;
		legacy.execute("VACUUM INTO ?1", [target])?;
	}
	let check: String =
		Connection::open(&staging)?.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
	if check != "ok" {
		remove_if_present(&staging)?;
		return Err(MigrationError::Corrupt(check));
	}
	// Under the lock the destination is known to be absent, so this cannot clobber it.
	fs::rename(&staging, new.join(DATABASE))?;
	Ok(())
}

/// Copy every regular file from `source` that is missing under `destination`. Existing
/// AscendCord files always win; symlinks are not followed.
fn copy_missing(source: &Path, destination: &Path) -> Result<usize, MigrationError> {
	if !source.is_dir() {
		return Ok(0);
	}
	fs::create_dir_all(destination)?;
	let mut copied = 0;
	for entry in fs::read_dir(source)? {
		let entry = entry?;
		let kind = entry.file_type()?;
		let target = destination.join(entry.file_name());
		if kind.is_dir() {
			copied += copy_missing(&entry.path(), &target)?;
		} else if kind.is_file() && !target.exists() {
			let staging = destination.join(format!(
				".{}.migrating",
				entry.file_name().to_string_lossy()
			));
			fs::copy(entry.path(), &staging)?;
			fs::rename(&staging, &target)?;
			copied += 1;
		}
	}
	Ok(copied)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
	match fs::remove_file(path) {
		Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
		_ => Ok(()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	struct Temp(PathBuf);
	impl Temp {
		fn new(name: &str) -> Self {
			let path = std::env::temp_dir().join(format!(
				"ascendcord-migration-{name}-{}-{:?}",
				std::process::id(),
				std::thread::current().id()
			));
			let _ = fs::remove_dir_all(&path);
			fs::create_dir_all(&path).unwrap();
			Self(path)
		}
	}
	impl Drop for Temp {
		fn drop(&mut self) {
			let _ = fs::remove_dir_all(&self.0);
		}
	}

	/// A legacy store whose newest commit is still only in the WAL file.
	fn wal_database(path: &Path) -> Connection {
		let db = Connection::open(path).unwrap();
		db.pragma_update(None, "journal_mode", "WAL").unwrap();
		db.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
		db.execute_batch(
			"CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
			 INSERT INTO settings VALUES('theme','midnight');",
		)
		.unwrap();
		db
	}

	fn value(path: &Path) -> String {
		Connection::open(path)
			.unwrap()
			.query_row("SELECT value FROM settings WHERE key='theme'", [], |row| {
				row.get(0)
			})
			.unwrap()
	}

	#[test]
	fn database_copy_includes_wal_commits_and_leaves_the_legacy_store() {
		let temp = Temp::new("wal");
		let (old, new) = (temp.0.join("tesktop2"), temp.0.join("AscendCord"));
		fs::create_dir_all(&old).unwrap();
		// Keep the writer open so the commit has not been checkpointed into the main file.
		let _writer = wal_database(&old.join(DATABASE));
		assert!(old.join("client.sqlite3-wal").is_file());
		let report = migrate(&old, &new).unwrap();
		assert!(report.database);
		assert_eq!(value(&new.join(DATABASE)), "midnight");
		assert!(old.join(DATABASE).is_file(), "copy, never move");
		assert!(!new.join(STAGING).exists());
	}

	#[test]
	fn only_durable_state_is_copied_and_newer_files_win() {
		let temp = Temp::new("files");
		let (old, new) = (temp.0.join("tesktop2"), temp.0.join("AscendCord"));
		fs::create_dir_all(old.join("extensions/theme")).unwrap();
		fs::create_dir_all(old.join("voice-bridge")).unwrap();
		fs::create_dir_all(old.join("crash-dumps")).unwrap();
		fs::write(old.join("extensions/theme/manifest.json"), b"legacy").unwrap();
		fs::write(old.join("extensions/kept.json"), b"legacy").unwrap();
		fs::write(old.join("voice-bridge/target.json"), b"{}").unwrap();
		fs::write(old.join("crash-dumps/one.dmp"), b"dump").unwrap();
		fs::write(old.join("detectable.json"), b"[]").unwrap();
		fs::write(old.join("tesktop-plugins.json"), b"{\"plugins\":{}}").unwrap();
		fs::create_dir_all(new.join("extensions")).unwrap();
		fs::write(new.join("extensions/kept.json"), b"new").unwrap();
		let report = migrate(&old, &new).unwrap();
		assert_eq!(report.extension_files, 1);
		assert_eq!(
			fs::read(new.join("extensions/theme/manifest.json")).unwrap(),
			b"legacy"
		);
		assert_eq!(fs::read(new.join("extensions/kept.json")).unwrap(), b"new");
		assert!(report.plugin_settings);
		assert_eq!(
			fs::read(new.join("testcord-plugins.json")).unwrap(),
			b"{\"plugins\":{}}"
		);
		for skipped in ["voice-bridge", "crash-dumps", "detectable.json"] {
			assert!(!new.join(skipped).exists(), "{skipped} must stay behind");
		}
		assert!(old.join("voice-bridge/target.json").is_file());
	}

	#[test]
	fn an_existing_database_is_never_replaced_and_reruns_do_nothing() {
		let temp = Temp::new("rerun");
		let (old, new) = (temp.0.join("tesktop2"), temp.0.join("AscendCord"));
		fs::create_dir_all(&old).unwrap();
		drop(wal_database(&old.join(DATABASE)));
		fs::create_dir_all(&new).unwrap();
		Connection::open(new.join(DATABASE))
			.unwrap()
			.execute_batch(
				"CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
				 INSERT INTO settings VALUES('theme','dawn');",
			)
			.unwrap();
		assert!(!migrate(&old, &new).unwrap().database);
		assert_eq!(value(&new.join(DATABASE)), "dawn");
		// Completed: a later legacy change is not pulled in again.
		fs::create_dir_all(old.join("extensions")).unwrap();
		fs::write(old.join("extensions/late.json"), b"x").unwrap();
		assert_eq!(migrate(&old, &new).unwrap(), Report::default());
		assert!(!new.join("extensions/late.json").exists());
	}

	#[test]
	fn a_failed_copy_keeps_the_new_folder_unused_until_a_retry_succeeds() {
		let temp = Temp::new("retry");
		let (old, new) = (temp.0.join("tesktop2"), temp.0.join("AscendCord"));
		fs::create_dir_all(&old).unwrap();
		drop(wal_database(&old.join(DATABASE)));
		let failed: Result<Report, MigrationError> =
			Err(MigrationError::Io(io::Error::other("disk full")));
		assert!(
			!usable(&failed, &old, &new),
			"no fresh store may shadow legacy data"
		);
		let retried = migrate(&old, &new);
		assert!(retried.as_ref().unwrap().database);
		assert!(usable(&retried, &old, &new));
		assert_eq!(value(&new.join(DATABASE)), "midnight");
		// Without legacy data, or once the copy exists, a later failure is not blocking.
		assert!(usable(&failed, &old, &new));
		assert!(usable(&failed, &temp.0.join("missing"), &new));
	}

	#[test]
	fn a_fresh_install_without_legacy_data_completes() {
		let temp = Temp::new("fresh");
		let new = temp.0.join("AscendCord");
		assert_eq!(
			migrate(&temp.0.join("missing"), &new).unwrap(),
			Report::default()
		);
		assert!(new.join(MARKER).is_file());
	}

	#[test]
	fn concurrent_starts_copy_the_database_once() {
		let temp = Temp::new("race");
		let (old, new) = (temp.0.join("tesktop2"), temp.0.join("AscendCord"));
		fs::create_dir_all(&old).unwrap();
		drop(wal_database(&old.join(DATABASE)));
		let copies: usize = std::thread::scope(|scope| {
			let runs: Vec<_> = (0..4)
				.map(|_| scope.spawn(|| migrate(&old, &new).unwrap().database))
				.collect();
			runs.into_iter()
				.map(|run| usize::from(run.join().unwrap()))
				.sum()
		});
		assert_eq!(copies, 1);
		assert_eq!(value(&new.join(DATABASE)), "midnight");
	}
}
