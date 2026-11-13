use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection};

use crate::settings::SearchEngine;

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

/// Source type for an omnibox autocomplete suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    History,
    Bookmark,
    Search,
}

impl SuggestionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Bookmark => "bookmark",
            Self::Search => "search",
        }
    }
}

/// A single ranked omnibox suggestion returned to the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct OmniboxSuggestion {
    pub kind: SuggestionKind,
    pub title: String,
    pub url: String,
    pub score: f64,
}

type SuggestReply = Box<dyn FnOnce(u64, Vec<OmniboxSuggestion>) + Send>;

enum DbJob {
    RecordVisit {
        url: String,
        title: String,
        now: i64,
    },
    UpdateVisitTitle {
        url: String,
        title: String,
    },
    Suggest {
        query_id: u64,
        query: String,
        limit: usize,
        online_enabled: bool,
        search_engine: SearchEngine,
        reply: SuggestReply,
    },
    Flush(mpsc::Sender<()>),
}

/// Persistent and default browser configuration and SQLite storage.
#[derive(Debug, Clone)]
pub struct Storage {
    webview_data_dir: PathBuf,
    filters_dir: PathBuf,
    downloads_dir: PathBuf,
    db: Arc<Mutex<Option<Connection>>>,
    worker_tx: mpsc::Sender<DbJob>,
    latest_query_id: Arc<AtomicU64>,
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

        let db = Arc::new(Mutex::new(conn));
        let latest_query_id = Arc::new(AtomicU64::new(0));
        let (worker_tx, worker_rx) = mpsc::channel::<DbJob>();

        let worker_db = Arc::clone(&db);
        let worker_latest_q = Arc::clone(&latest_query_id);
        let _ = thread::Builder::new()
            .name(String::from("ember-db-worker"))
            .spawn(move || {
                while let Ok(job) = worker_rx.recv() {
                    match job {
                        DbJob::RecordVisit { url, title, now } => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_record_visit(c, &url, &title, now);
                                }
                            }
                        }
                        DbJob::UpdateVisitTitle { url, title } => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_update_visit_title(c, &url, &title);
                                }
                            }
                        }
                        DbJob::Suggest {
                            query_id,
                            query,
                            limit,
                            online_enabled,
                            search_engine,
                            reply,
                        } => {
                            // Skip stale queries that have already been superseded by newer typing.
                            if query_id < worker_latest_q.load(Ordering::Relaxed) {
                                continue;
                            }
                            let (history, bookmarks) = if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    query_suggestion_candidates(c, &query, 48)
                                } else {
                                    (Vec::new(), Vec::new())
                                }
                            } else {
                                (Vec::new(), Vec::new())
                            };
                            if query_id < worker_latest_q.load(Ordering::Relaxed) {
                                continue;
                            }
                            let mut suggestions = rank_suggestions(
                                &query,
                                &history,
                                &bookmarks,
                                unix_timestamp_secs(),
                                limit,
                            );
                            if online_enabled && suggestions.len() < limit {
                                let trimmed = query.trim();
                                let lower = trimmed.to_ascii_lowercase();
                                let is_url_like = lower.starts_with("http://")
                                    || lower.starts_with("https://")
                                    || lower.starts_with("ember://");
                                if !is_url_like && trimmed.chars().count() >= 2 {
                                    let room = limit.saturating_sub(suggestions.len()).min(4);
                                    let online = fetch_online_search_suggestions(
                                        trimmed,
                                        search_engine,
                                        room,
                                    );
                                    if query_id < worker_latest_q.load(Ordering::Relaxed) {
                                        continue;
                                    }
                                    for phrase in online {
                                        let search_url = search_engine.build_search_url(&phrase);
                                        if !suggestions.iter().any(|s| {
                                            s.title.eq_ignore_ascii_case(&phrase)
                                                || s.url == search_url
                                        }) {
                                            suggestions.push(OmniboxSuggestion {
                                                kind: SuggestionKind::Search,
                                                title: phrase,
                                                url: search_url,
                                                score: 10.0,
                                            });
                                        }
                                    }
                                }
                            }
                            reply(query_id, suggestions);
                        }
                        DbJob::Flush(tx) => {
                            let _ = tx.send(());
                        }
                    }
                }
            });

        Ok(Self {
            webview_data_dir,
            filters_dir,
            downloads_dir,
            db,
            worker_tx,
            latest_query_id,
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
            CREATE INDEX IF NOT EXISTS idx_history_url_nocase ON history(url COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_history_title_nocase ON history(title COLLATE NOCASE);

            CREATE TABLE IF NOT EXISTS bookmarks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bookmarks_created_at ON bookmarks(created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_bookmarks_url_nocase ON bookmarks(url COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_bookmarks_title_nocase ON bookmarks(title COLLATE NOCASE);

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

    /// Records a visit to `url` with `title` in the SQLite `history` table synchronously.
    /// Internal `ember://`, `about:`, and `data:` URLs are skipped.
    pub fn record_visit(&self, url: &str, title: &str) {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return;
        }
        let clean_title = sanitize_history_title(trimmed, title);
        let now = unix_timestamp_secs() as i64;
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                execute_record_visit(conn, trimmed, clean_title, now);
            }
        }
    }

    /// Enqueues a non-blocking history write onto the background DB worker thread.
    pub fn record_visit_async(&self, url: &str, title: &str) {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return;
        }
        let clean_title = sanitize_history_title(trimmed, title);
        let now = unix_timestamp_secs() as i64;
        let _ = self.worker_tx.send(DbJob::RecordVisit {
            url: trimmed.to_owned(),
            title: clean_title.to_owned(),
            now,
        });
    }

    /// Updates the stored title for an already-visited `url` synchronously.
    pub fn update_visit_title(&self, url: &str, title: &str) {
        let trimmed_url = url.trim();
        let trimmed_title = title.trim();
        if !should_track_url(trimmed_url) || trimmed_title.is_empty() {
            return;
        }
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                execute_update_visit_title(conn, trimmed_url, trimmed_title);
            }
        }
    }

    /// Enqueues a non-blocking title update onto the background DB worker thread.
    pub fn update_visit_title_async(&self, url: &str, title: &str) {
        let trimmed_url = url.trim();
        let trimmed_title = title.trim();
        if !should_track_url(trimmed_url) || trimmed_title.is_empty() {
            return;
        }
        let _ = self.worker_tx.send(DbJob::UpdateVisitTitle {
            url: trimmed_url.to_owned(),
            title: trimmed_title.to_owned(),
        });
    }

    /// Schedules an asynchronous omnibox suggestion query on the background DB worker.
    /// Any older in-flight query with a lower `query_id` is cancelled automatically.
    pub fn suggest_async<F>(
        &self,
        query_id: u64,
        query: String,
        limit: usize,
        online_enabled: bool,
        search_engine: SearchEngine,
        reply: F,
    ) where
        F: FnOnce(u64, Vec<OmniboxSuggestion>) + Send + 'static,
    {
        self.latest_query_id.store(query_id, Ordering::Relaxed);
        let _ = self.worker_tx.send(DbJob::Suggest {
            query_id,
            query,
            limit,
            online_enabled,
            search_engine,
            reply: Box::new(reply),
        });
    }

    /// Blocks until all previously enqueued background DB jobs have completed.
    pub fn flush_worker(&self) {
        let (tx, rx) = mpsc::channel();
        if self.worker_tx.send(DbJob::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
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

fn sanitize_history_title<'a>(url: &'a str, title: &'a str) -> &'a str {
    let t = title.trim();
    if t.is_empty() || t == "New Tab" {
        url
    } else {
        t
    }
}

fn execute_record_visit(conn: &Connection, url: &str, title: &str, now: i64) {
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
        params![url, title, now],
    );
}

fn execute_update_visit_title(conn: &Connection, url: &str, title: &str) {
    let _ = conn.execute(
        "UPDATE history SET title = ?1 WHERE url = ?2",
        params![title, url],
    );
}

fn escape_like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 4);
    for ch in q.chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            c => out.push(c),
        }
    }
    out
}

fn query_suggestion_candidates(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> (Vec<HistoryEntry>, Vec<BookmarkEntry>) {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let pattern = format!("%{}%", escape_like_pattern(trimmed));
    let cap = limit as i64;

    let mut history = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, url, title, visit_count, last_visited_at
         FROM history
         WHERE url LIKE ?1 ESCAPE '\\' COLLATE NOCASE
            OR title LIKE ?1 ESCAPE '\\' COLLATE NOCASE
         ORDER BY visit_count DESC, last_visited_at DESC
         LIMIT ?2",
    ) {
        if let Ok(rows) = stmt.query_map(params![pattern, cap], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                visit_count: row.get(3)?,
                last_visited_at: row.get::<_, i64>(4)?.max(0) as u64,
            })
        }) {
            for row in rows.flatten() {
                history.push(row);
            }
        }
    }

    let mut bookmarks = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, url, title, created_at
         FROM bookmarks
         WHERE url LIKE ?1 ESCAPE '\\' COLLATE NOCASE
            OR title LIKE ?1 ESCAPE '\\' COLLATE NOCASE
         ORDER BY created_at DESC
         LIMIT ?2",
    ) {
        if let Ok(rows) = stmt.query_map(params![pattern, cap], |row| {
            Ok(BookmarkEntry {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                created_at: row.get::<_, i64>(3)?.max(0) as u64,
            })
        }) {
            for row in rows.flatten() {
                bookmarks.push(row);
            }
        }
    }

    (history, bookmarks)
}

fn strip_url_scheme_and_www(url_lower: &str) -> &str {
    let rest = url_lower
        .strip_prefix("https://")
        .or_else(|| url_lower.strip_prefix("http://"))
        .unwrap_or(url_lower);
    rest.strip_prefix("www.").unwrap_or(rest)
}

/// Ranks history and bookmark matches by prefix match, visit count, and recency.
pub fn rank_suggestions(
    query: &str,
    history: &[HistoryEntry],
    bookmarks: &[BookmarkEntry],
    now_secs: u64,
    limit: usize,
) -> Vec<OmniboxSuggestion> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut by_url: HashMap<String, OmniboxSuggestion> = HashMap::new();

    for entry in history {
        let url_lower = entry.url.to_ascii_lowercase();
        let host_path = strip_url_scheme_and_www(&url_lower);
        let title_lower = entry.title.to_ascii_lowercase();

        let match_score = if host_path.starts_with(&q) {
            1000.0
        } else if url_lower.starts_with(&q) {
            950.0
        } else if title_lower.starts_with(&q) {
            700.0
        } else if host_path.contains(&q) {
            350.0
        } else if title_lower.contains(&q) {
            250.0
        } else {
            continue;
        };

        let visit_boost = (entry.visit_count.max(0) as f64).ln_1p() * 90.0;
        let age_hours = now_secs.saturating_sub(entry.last_visited_at) as f64 / 3600.0;
        let recency_boost = 150.0 / (1.0 + age_hours / 24.0);
        let score = match_score + visit_boost + recency_boost;

        by_url.insert(
            entry.url.clone(),
            OmniboxSuggestion {
                kind: SuggestionKind::History,
                title: entry.title.clone(),
                url: entry.url.clone(),
                score,
            },
        );
    }

    for bm in bookmarks {
        let url_lower = bm.url.to_ascii_lowercase();
        let host_path = strip_url_scheme_and_www(&url_lower);
        let title_lower = bm.title.to_ascii_lowercase();

        let match_score = if host_path.starts_with(&q) {
            1000.0
        } else if url_lower.starts_with(&q) {
            950.0
        } else if title_lower.starts_with(&q) {
            700.0
        } else if host_path.contains(&q) {
            350.0
        } else if title_lower.contains(&q) {
            250.0
        } else {
            continue;
        };

        let bookmark_bonus = 180.0;
        if let Some(existing) = by_url.get_mut(&bm.url) {
            existing.kind = SuggestionKind::Bookmark;
            existing.score += bookmark_bonus;
            if !bm.title.trim().is_empty() {
                existing.title.clone_from(&bm.title);
            }
        } else {
            by_url.insert(
                bm.url.clone(),
                OmniboxSuggestion {
                    kind: SuggestionKind::Bookmark,
                    title: if bm.title.trim().is_empty() {
                        bm.url.clone()
                    } else {
                        bm.title.clone()
                    },
                    url: bm.url.clone(),
                    score: match_score + bookmark_bonus,
                },
            );
        }
    }

    let mut out: Vec<OmniboxSuggestion> = by_url.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.url.cmp(&b.url))
    });
    out.truncate(limit);
    out
}

fn percent_encode_query(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => {
                out.push('%');
                let hi = (other >> 4) & 0x0f;
                let lo = other & 0x0f;
                out.push(char::from_digit(u32::from(hi), 16).unwrap_or('0'));
                out.push(char::from_digit(u32::from(lo), 16).unwrap_or('0'));
            }
        }
    }
    out
}

fn fetch_online_search_suggestions(query: &str, engine: SearchEngine, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let encoded = percent_encode_query(query);
    let url = match engine {
        SearchEngine::DuckDuckGo => format!("https://duckduckgo.com/ac/?q={encoded}"),
        SearchEngine::Google => {
            format!("https://suggestqueries.google.com/complete/search?client=firefox&q={encoded}")
        }
        SearchEngine::Brave => format!("https://search.brave.com/api/suggest?q={encoded}"),
        SearchEngine::Bing => format!("https://api.bing.com/osjson.aspx?query={encoded}"),
        SearchEngine::Startpage => {
            format!("https://www.startpage.com/osuggestions?q={encoded}")
        }
    };
    let Some(body) = platform::http_get_utf8(&url) else {
        return Vec::new();
    };
    match engine {
        SearchEngine::DuckDuckGo => parse_ddg_autocomplete(&body, limit),
        SearchEngine::Google
        | SearchEngine::Brave
        | SearchEngine::Bing
        | SearchEngine::Startpage => parse_opensearch_autocomplete(&body, limit),
    }
}

fn parse_ddg_autocomplete(body: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let needle = "\"phrase\":\"";
    let mut pos = 0usize;
    while out.len() < limit {
        let Some(found) = body[pos..].find(needle) else {
            break;
        };
        let start = pos + found + needle.len();
        if let Some((decoded, next_pos)) = read_json_string_content(&body[start..]) {
            if !decoded.trim().is_empty() {
                out.push(decoded);
            }
            pos = start + next_pos;
        } else {
            break;
        }
    }
    out
}

fn parse_opensearch_autocomplete(body: &str, limit: usize) -> Vec<String> {
    // Format: ["query", ["suggestion1", "suggestion2", ...]]
    let Some(first_bracket) = body.find('[') else {
        return Vec::new();
    };
    let Some(inner_bracket) = body[first_bracket + 1..].find('[') else {
        return Vec::new();
    };
    let arr_start = first_bracket + 1 + inner_bracket + 1;
    let slice = &body[arr_start..];
    let mut out = Vec::new();
    let mut i = 0usize;
    let bytes = slice.as_bytes();
    while i < bytes.len() && out.len() < limit {
        if bytes[i] == b']' {
            break;
        }
        if bytes[i] == b'"' {
            if let Some((decoded, consumed)) = read_json_string_content(&slice[i + 1..]) {
                if !decoded.trim().is_empty() {
                    out.push(decoded);
                }
                i += 1 + consumed + 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    out
}

fn read_json_string_content(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut escaped = false;
    for (idx, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            match ch {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            }
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some((out, idx));
        } else {
            out.push(ch);
        }
    }
    None
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

    #[link(name = "wininet", kind = "raw-dylib")]
    extern "system" {
        fn InternetOpenW(
            lpsz_agent: *const u16,
            dw_access_type: u32,
            lpsz_proxy: *const u16,
            lpsz_proxy_bypass: *const u16,
            dw_flags: u32,
        ) -> isize;
        fn InternetOpenUrlW(
            h_internet: isize,
            lpsz_url: *const u16,
            lpsz_headers: *const u16,
            dw_headers_length: u32,
            dw_flags: u32,
            dw_context: usize,
        ) -> isize;
        fn InternetReadFile(
            h_file: isize,
            lp_buffer: *mut u8,
            dw_number_of_bytes_to_read: u32,
            lpdw_number_of_bytes_read: *mut u32,
        ) -> i32;
        fn InternetCloseHandle(h_internet: isize) -> i32;
    }

    pub fn http_get_utf8(url: &str) -> Option<String> {
        let agent: Vec<u16> = "Ember/0.1\0".encode_utf16().collect();
        let url_wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        const INTERNET_OPEN_TYPE_PRECONFIG: u32 = 0;
        const INTERNET_FLAG_NO_UI: u32 = 0x0000_0200;
        const INTERNET_FLAG_SECURE: u32 = 0x0080_0000;

        // SAFETY: `agent` and `url_wide` are null-terminated UTF-16 buffers valid for the call.
        let h_inet = unsafe {
            InternetOpenW(
                agent.as_ptr(),
                INTERNET_OPEN_TYPE_PRECONFIG,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        };
        if h_inet == 0 {
            return None;
        }
        let h_url = unsafe {
            InternetOpenUrlW(
                h_inet,
                url_wide.as_ptr(),
                std::ptr::null(),
                0,
                INTERNET_FLAG_NO_UI | INTERNET_FLAG_SECURE,
                0,
            )
        };
        if h_url == 0 {
            unsafe { InternetCloseHandle(h_inet) };
            return None;
        }

        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 2048];
        loop {
            let mut bytes_read: u32 = 0;
            let ok = unsafe {
                InternetReadFile(
                    h_url,
                    chunk.as_mut_ptr(),
                    chunk.len() as u32,
                    &mut bytes_read,
                )
            };
            if ok == 0 || bytes_read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..bytes_read as usize]);
            if buf.len() >= 64 * 1024 {
                break;
            }
        }
        unsafe {
            InternetCloseHandle(h_url);
            InternetCloseHandle(h_inet);
        }
        String::from_utf8(buf).ok()
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

    pub fn http_get_utf8(_url: &str) -> Option<String> {
        None
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

    pub fn http_get_utf8(_url: &str) -> Option<String> {
        None
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

    #[test]
    fn test_omnibox_suggestion_ranking() {
        let now = 1_700_000_000u64;
        let history = vec![
            HistoryEntry {
                id: 1,
                url: String::from("https://example.com/search?q=rust"),
                title: String::from("Search results"),
                visit_count: 20,
                last_visited_at: now - 10,
            },
            HistoryEntry {
                id: 2,
                url: String::from("https://www.rust-lang.org/"),
                title: String::from("Rust Programming Language"),
                visit_count: 5,
                last_visited_at: now - 3600,
            },
            HistoryEntry {
                id: 3,
                url: String::from("https://rustlings.cool/"),
                title: String::from("Rustlings"),
                visit_count: 1,
                last_visited_at: now - 86_400 * 10,
            },
        ];
        let bookmarks = vec![BookmarkEntry {
            id: 1,
            url: String::from("https://www.rust-lang.org/"),
            title: String::from("Rust Programming Language"),
            created_at: now - 100,
        }];

        let ranked = rank_suggestions("rust", &history, &bookmarks, now, 8);
        assert_eq!(ranked.len(), 3);
        // Prefix match + bookmark bonus beats substring match even with higher visit_count
        assert_eq!(ranked[0].url, "https://www.rust-lang.org/");
        assert_eq!(ranked[0].kind, SuggestionKind::Bookmark);
        assert_eq!(ranked[1].url, "https://rustlings.cool/");
        assert_eq!(ranked[2].url, "https://example.com/search?q=rust");

        // Cap at limit
        let capped = rank_suggestions("rust", &history, &bookmarks, now, 2);
        assert_eq!(capped.len(), 2);
    }

    #[test]
    fn test_async_history_worker_and_dedup_updates() {
        let storage = temp_storage("async_worker");

        // Internal and non-HTTP URLs must be ignored
        storage.record_visit_async("ember://settings", "Settings");
        storage.record_visit_async("about:blank", "Blank");
        storage.record_visit_async("https://docs.rs/tokio", "New Tab");
        storage.record_visit_async("https://docs.rs/tokio", "tokio - Rust");
        storage.update_visit_title_async("https://docs.rs/tokio", "Tokio Crate Docs");
        storage.flush_worker();

        let entries = storage.recent_history(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://docs.rs/tokio");
        assert_eq!(entries[0].title, "Tokio Crate Docs");
        assert_eq!(entries[0].visit_count, 2);

        // Verify async suggestion query returns the entry
        let (tx, rx) = mpsc::channel();
        storage.suggest_async(
            42,
            String::from("docs.rs"),
            8,
            false,
            SearchEngine::DuckDuckGo,
            move |qid, items| {
                let _ = tx.send((qid, items));
            },
        );
        let (qid, items) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("suggestions");
        assert_eq!(qid, 42);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://docs.rs/tokio");
        assert_eq!(items[0].kind, SuggestionKind::History);

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }
}
