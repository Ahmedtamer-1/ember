use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection};

/// Default inactivity duration (in minutes) before a background tab's WebView is suspended.
pub const DEFAULT_TAB_SUSPEND_MINUTES: u64 = 5;

/// A single browsing history record stored in SQLite (`ember.db`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub last_visited_at: u64,
}

/// A single bookmark record stored in SQLite (`ember.db`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub created_at: u64,
}

/// Persistent and default browser configuration and SQLite storage.
#[derive(Debug, Clone)]
pub struct Storage {
    webview_data_dir: PathBuf,
    filters_dir: PathBuf,
    downloads_dir: PathBuf,
    db: Arc<Mutex<Option<Connection>>>,
}

impl Storage {
    /// Initializes browser storage, ensures per-user directories exist, and opens `%LOCALAPPDATA%\Ember\ember.db`.
    pub fn init() -> io::Result<Self> {
        let base_dir = platform::resolve_app_data_dir()?;
        Self::with_base_dir(base_dir)
    }

    /// Initializes storage rooted at a specific directory (used by `init` and unit tests).
    pub fn with_base_dir(base_dir: PathBuf) -> io::Result<Self> {
        let webview_data_dir = base_dir.join("webview-data");
        let filters_dir = base_dir.join("filters");
        let downloads_dir =
            platform::resolve_downloads_dir().unwrap_or_else(|_| base_dir.join("Downloads"));
        fs::create_dir_all(&webview_data_dir)?;
        fs::create_dir_all(&filters_dir)?;
        let _ = fs::create_dir_all(&downloads_dir);

        let db_path = base_dir.join("ember.db");
        let conn = Connection::open(&db_path).ok();
        if let Some(ref c) = conn {
            Self::init_schema(c);
        }

        Ok(Self {
            webview_data_dir,
            filters_dir,
            downloads_dir,
            db: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) {
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                visit_count INTEGER NOT NULL DEFAULT 1,
                last_visited_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_last_visited ON history(last_visited_at DESC);
            CREATE INDEX IF NOT EXISTS idx_history_visit_count ON history(visit_count DESC, last_visited_at DESC);

            CREATE TABLE IF NOT EXISTS bookmarks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bookmarks_created_at ON bookmarks(created_at DESC);

            CREATE TABLE IF NOT EXISTS site_zoom (
                host TEXT PRIMARY KEY,
                zoom_factor REAL NOT NULL
            );",
        );
    }

    /// Returns the isolated per-user WebView data directory (`%LOCALAPPDATA%\Ember\webview-data` on Windows).
    pub fn webview_data_dir(&self) -> &Path {
        &self.webview_data_dir
    }

    /// Returns the per-user adblock filter and serialized engine directory (`%LOCALAPPDATA%\Ember\filters` on Windows).
    pub fn filters_dir(&self) -> &Path {
        &self.filters_dir
    }

    /// Returns the user's Downloads directory (`%USERPROFILE%\Downloads` on Windows).
    pub fn downloads_dir(&self) -> &Path {
        &self.downloads_dir
    }

    /// Records a visit to `url` with `title` in the SQLite `history` table.
    /// Internal `ember://`, `about:`, and `data:` URLs are skipped.
    pub fn record_visit(&self, url: &str, title: &str) {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return;
        }
        let clean_title = if title.trim().is_empty() || title.trim() == "New Tab" {
            trimmed
        } else {
            title.trim()
        };
        let now = unix_timestamp_secs() as i64;
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute(
                    "INSERT INTO history (url, title, visit_count, last_visited_at)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(url) DO UPDATE SET
                        title = CASE
                            WHEN excluded.title != excluded.url THEN excluded.title
                            ELSE history.title
                        END,
                        visit_count = history.visit_count + 1,
                        last_visited_at = excluded.last_visited_at",
                    params![trimmed, clean_title, now],
                );
            }
        }
    }

    /// Updates the stored title for an already-visited `url` once `document.title` resolves.
    pub fn update_visit_title(&self, url: &str, title: &str) {
        let trimmed_url = url.trim();
        let trimmed_title = title.trim();
        if !should_track_url(trimmed_url) || trimmed_title.is_empty() {
            return;
        }
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute(
                    "UPDATE history SET title = ?1 WHERE url = ?2",
                    params![trimmed_title, trimmed_url],
                );
            }
        }
    }

    /// Returns up to `limit` most recently visited pages ordered by `last_visited_at DESC`.
    pub fn recent_history(&self, limit: usize) -> Vec<HistoryEntry> {
        let mut results = Vec::new();
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, url, title, visit_count, last_visited_at
                     FROM history
                     ORDER BY last_visited_at DESC, id DESC
                     LIMIT ?1",
                ) {
                    if let Ok(rows) = stmt.query_map(params![limit as i64], |row| {
                        Ok(HistoryEntry {
                            id: row.get(0)?,
                            url: row.get(1)?,
                            title: row.get(2)?,
                            visit_count: row.get(3)?,
                            last_visited_at: row.get::<_, i64>(4)?.max(0) as u64,
                        })
                    }) {
                        for item in rows.flatten() {
                            results.push(item);
                        }
                    }
                }
            }
        }
        results
    }

    /// Returns up to `limit` most frequently visited sites ordered by `visit_count DESC, last_visited_at DESC`.
    pub fn most_visited(&self, limit: usize) -> Vec<HistoryEntry> {
        let mut results = Vec::new();
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, url, title, visit_count, last_visited_at
                     FROM history
                     ORDER BY visit_count DESC, last_visited_at DESC
                     LIMIT ?1",
                ) {
                    if let Ok(rows) = stmt.query_map(params![limit as i64], |row| {
                        Ok(HistoryEntry {
                            id: row.get(0)?,
                            url: row.get(1)?,
                            title: row.get(2)?,
                            visit_count: row.get(3)?,
                            last_visited_at: row.get::<_, i64>(4)?.max(0) as u64,
                        })
                    }) {
                        for item in rows.flatten() {
                            results.push(item);
                        }
                    }
                }
            }
        }
        results
    }

    /// Deletes a single URL from browsing history.
    pub fn delete_history_url(&self, url: &str) {
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute("DELETE FROM history WHERE url = ?1", params![url.trim()]);
            }
        }
    }

    /// Clears all browsing history from SQLite.
    pub fn clear_history(&self) {
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute("DELETE FROM history", []);
            }
        }
    }

    /// Returns `true` if `url` is currently bookmarked.
    pub fn is_bookmarked(&self, url: &str) -> bool {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return false;
        }
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if let Ok(mut stmt) = conn.prepare("SELECT 1 FROM bookmarks WHERE url = ?1 LIMIT 1")
                {
                    if let Ok(exists) = stmt.exists(params![trimmed]) {
                        return exists;
                    }
                }
            }
        }
        false
    }

    /// Toggles a bookmark for `url`. Returns `true` if the page is now bookmarked, or `false` if removed.
    pub fn toggle_bookmark(&self, url: &str, title: &str) -> bool {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return false;
        }
        if self.is_bookmarked(trimmed) {
            self.remove_bookmark(trimmed);
            false
        } else {
            self.add_bookmark(trimmed, title);
            true
        }
    }

    /// Adds or updates a bookmark for `url`.
    pub fn add_bookmark(&self, url: &str, title: &str) {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return;
        }
        let clean_title = if title.trim().is_empty() {
            trimmed
        } else {
            title.trim()
        };
        let now = unix_timestamp_secs() as i64;
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute(
                    "INSERT INTO bookmarks (url, title, created_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(url) DO UPDATE SET title = excluded.title",
                    params![trimmed, clean_title, now],
                );
            }
        }
    }

    /// Removes a bookmark by URL.
    pub fn remove_bookmark(&self, url: &str) {
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute("DELETE FROM bookmarks WHERE url = ?1", params![url.trim()]);
            }
        }
    }

    /// Returns all saved bookmarks ordered newest first.
    pub fn list_bookmarks(&self) -> Vec<BookmarkEntry> {
        let mut results = Vec::new();
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT id, url, title, created_at
                     FROM bookmarks
                     ORDER BY created_at DESC, id DESC",
                ) {
                    if let Ok(rows) = stmt.query_map([], |row| {
                        Ok(BookmarkEntry {
                            id: row.get(0)?,
                            url: row.get(1)?,
                            title: row.get(2)?,
                            created_at: row.get::<_, i64>(3)?.max(0) as u64,
                        })
                    }) {
                        for item in rows.flatten() {
                            results.push(item);
                        }
                    }
                }
            }
        }
        results
    }

    /// Looks up the remembered zoom factor for `host` (defaults to `1.0`).
    pub fn get_site_zoom(&self, host: &str) -> f64 {
        let clean_host = host.trim().to_ascii_lowercase();
        if clean_host.is_empty() {
            return 1.0;
        }
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if let Ok(mut stmt) =
                    conn.prepare("SELECT zoom_factor FROM site_zoom WHERE host = ?1 LIMIT 1")
                {
                    if let Ok(factor) =
                        stmt.query_row(params![clean_host], |row| row.get::<_, f64>(0))
                    {
                        if factor.is_finite() && (0.25..=5.0).contains(&factor) {
                            return factor;
                        }
                    }
                }
            }
        }
        1.0
    }

    /// Persists the zoom factor for `host` in SQLite (or removes the row if reset to `1.0`).
    pub fn set_site_zoom(&self, host: &str, zoom_factor: f64) {
        let clean_host = host.trim().to_ascii_lowercase();
        if clean_host.is_empty() || !zoom_factor.is_finite() {
            return;
        }
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                if (zoom_factor - 1.0).abs() < 0.005 {
                    let _ =
                        conn.execute("DELETE FROM site_zoom WHERE host = ?1", params![clean_host]);
                } else {
                    let _ = conn.execute(
                        "INSERT INTO site_zoom (host, zoom_factor)
                         VALUES (?1, ?2)
                         ON CONFLICT(host) DO UPDATE SET zoom_factor = excluded.zoom_factor",
                        params![clean_host, zoom_factor],
                    );
                }
            }
        }
    }
}

fn should_track_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !lower.contains("?ember_bench=")
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_app_data_dir() -> io::Result<PathBuf> {
        let local_app_data = env::var_os("LOCALAPPDATA").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Environment variable %LOCALAPPDATA% is not set",
            )
        })?;

        Ok(PathBuf::from(local_app_data).join("Ember"))
    }

    pub fn resolve_downloads_dir() -> io::Result<PathBuf> {
        if let Some(profile) = env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(profile).join("Downloads"));
        }
        let local_app_data = resolve_app_data_dir()?;
        Ok(local_app_data.join("Downloads"))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_app_data_dir() -> io::Result<PathBuf> {
        let home = env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Environment variable $HOME is not set",
            )
        })?;

        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Ember"))
    }

    pub fn resolve_downloads_dir() -> io::Result<PathBuf> {
        let home = env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Environment variable $HOME is not set",
            )
        })?;
        Ok(PathBuf::from(home).join("Downloads"))
    }
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_app_data_dir() -> io::Result<PathBuf> {
        if let Some(xdg_data) = env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg_data).join("Ember"));
        }

        let home = env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Neither $XDG_DATA_HOME nor $HOME is set",
            )
        })?;

        Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("Ember"))
    }

    pub fn resolve_downloads_dir() -> io::Result<PathBuf> {
        let home = env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Environment variable $HOME is not set",
            )
        })?;
        Ok(PathBuf::from(home).join("Downloads"))
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    fn temp_storage(test_name: &str) -> Storage {
        let dir = env::temp_dir().join(format!(
            "ember_storage_test_{}_{}",
            test_name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        Storage::with_base_dir(dir).expect("temp storage should initialize")
    }

    #[test]
    fn test_sqlite_history_and_most_visited() {
        let storage = temp_storage("history");

        storage.record_visit("https://rust-lang.org", "Rust Programming Language");
        storage.record_visit("https://docs.rs", "Docs.rs");
        storage.record_visit("https://rust-lang.org", "Rust Programming Language");
        storage.record_visit("ember://newtab", "New Tab"); // internal URL ignored

        let recent = storage.recent_history(10);
        assert_eq!(recent.len(), 2);

        let most = storage.most_visited(10);
        assert_eq!(most.len(), 2);
        assert_eq!(most[0].url, "https://rust-lang.org");
        assert_eq!(most[0].visit_count, 2);
        assert_eq!(most[1].url, "https://docs.rs");
        assert_eq!(most[1].visit_count, 1);

        storage.update_visit_title("https://docs.rs", "Docs.rs â€” Rust Crate Docs");
        let updated = storage.recent_history(10);
        assert!(updated
            .iter()
            .any(|e| e.url == "https://docs.rs" && e.title == "Docs.rs â€” Rust Crate Docs"));

        storage.clear_history();
        assert!(storage.recent_history(10).is_empty());
        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    #[test]
    fn test_sqlite_bookmarks_and_site_zoom() {
        let storage = temp_storage("bookmarks_zoom");

        assert!(!storage.is_bookmarked("https://crates.io"));
        assert!(storage.toggle_bookmark("https://crates.io", "crates.io: Rust Package Registry"));
        assert!(storage.is_bookmarked("https://crates.io"));

        let bookmarks = storage.list_bookmarks();
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].url, "https://crates.io");
        assert_eq!(bookmarks[0].title, "crates.io: Rust Package Registry");

        // Toggling again removes it
        assert!(!storage.toggle_bookmark("https://crates.io", "crates.io"));
        assert!(!storage.is_bookmarked("https://crates.io"));
        assert!(storage.list_bookmarks().is_empty());

        // Per-site zoom persistence
        assert!((storage.get_site_zoom("example.com") - 1.0).abs() < 1e-6);
        storage.set_site_zoom("Example.COM", 1.25);
        assert!((storage.get_site_zoom("example.com") - 1.25).abs() < 1e-6);

        // Resetting to 1.0 removes the custom zoom entry
        storage.set_site_zoom("example.com", 1.0);
        assert!((storage.get_site_zoom("example.com") - 1.0).abs() < 1e-6);

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }
}
