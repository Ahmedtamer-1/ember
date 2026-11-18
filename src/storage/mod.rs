use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex, RwLock,
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
    pub is_default: bool,
}

/// Inline autocomplete result for the omnibox input field.
#[derive(Debug, Clone, PartialEq)]
pub struct AutofillMatch {
    pub completed_text: String,
    pub full_url: String,
    pub title: String,
    pub kind: SuggestionKind,
    pub score: f64,
}

const OMNIBOX_CACHE_MAX_ENTRIES: usize = 4096;

/// Pre-normalized history/bookmark entry stored in the in-memory omnibox cache.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedUrlEntry {
    pub url: String,
    pub url_lower: String,
    pub host_no_www: String,
    pub host_path_no_www: String,
    pub title: String,
    pub visit_count: i64,
    pub typed_count: i64,
    pub last_visited_at: u64,
    pub is_bookmarked: bool,
}

impl CachedUrlEntry {
    fn from_parts(
        url: String,
        title: String,
        visit_count: i64,
        typed_count: i64,
        last_visited_at: u64,
        is_bookmarked: bool,
    ) -> Self {
        let url_lower = url.to_ascii_lowercase();
        let host_path_no_www = strip_url_scheme_and_www(&url_lower).to_owned();
        let host_no_www = extract_host_from_stripped(&host_path_no_www).to_owned();
        Self {
            url,
            url_lower,
            host_no_www,
            host_path_no_www,
            title,
            visit_count,
            typed_count,
            last_visited_at,
            is_bookmarked,
        }
    }
}

#[derive(Debug, Default)]
struct OmniboxCache {
    entries: HashMap<String, CachedUrlEntry>,
}

impl OmniboxCache {
    fn warm_from_db(&mut self, conn: &Connection) {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT url, title, visit_count, typed_count, last_visited_at
             FROM history
             ORDER BY (typed_count * 3 + visit_count) DESC, last_visited_at DESC
             LIMIT ?1",
        ) {
            if let Ok(rows) =
                stmt.query_map(params![(OMNIBOX_CACHE_MAX_ENTRIES - 512) as i64], |row| {
                    let url: String = row.get(0)?;
                    let title: String = row.get(1)?;
                    let visit_count: i64 = row.get(2)?;
                    let typed_count: i64 = row.get(3)?;
                    let last_visited_at: u64 = row.get::<_, i64>(4)?.max(0) as u64;
                    Ok((url, title, visit_count, typed_count, last_visited_at))
                })
            {
                for (url, title, visit_count, typed_count, last_visited_at) in rows.flatten() {
                    self.entries.entry(url.clone()).or_insert_with(|| {
                        CachedUrlEntry::from_parts(
                            url,
                            title,
                            visit_count,
                            typed_count,
                            last_visited_at,
                            false,
                        )
                    });
                }
            }
        }

        if let Ok(mut stmt) = conn.prepare(
            "SELECT url, title, created_at
             FROM bookmarks
             ORDER BY created_at DESC",
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                let url: String = row.get(0)?;
                let title: String = row.get(1)?;
                let created_at: u64 = row.get::<_, i64>(2)?.max(0) as u64;
                Ok((url, title, created_at))
            }) {
                for (url, title, created_at) in rows.flatten() {
                    if let Some(existing) = self.entries.get_mut(&url) {
                        existing.is_bookmarked = true;
                        if !title.trim().is_empty() {
                            existing.title = title;
                        }
                    } else {
                        let entry =
                            CachedUrlEntry::from_parts(url.clone(), title, 0, 0, created_at, true);
                        self.entries.insert(url, entry);
                    }
                }
            }
        }
        self.prune_if_needed();
    }

    fn upsert_visit(&mut self, url: &str, title: &str, now_secs: u64, is_typed: bool) {
        if let Some(existing) = self.entries.get_mut(url) {
            existing.visit_count += 1;
            if is_typed {
                existing.typed_count += 1;
            }
            existing.last_visited_at = now_secs;
            if title != url && !title.trim().is_empty() {
                existing.title = title.to_owned();
            }
        } else {
            let entry = CachedUrlEntry::from_parts(
                url.to_owned(),
                title.to_owned(),
                1,
                if is_typed { 1 } else { 0 },
                now_secs,
                false,
            );
            self.entries.insert(url.to_owned(), entry);
            self.prune_if_needed();
        }
    }

    fn mark_typed(&mut self, url: &str, now_secs: u64) {
        if let Some(existing) = self.entries.get_mut(url) {
            existing.typed_count += 1;
            existing.last_visited_at = existing.last_visited_at.max(now_secs);
        } else {
            let entry =
                CachedUrlEntry::from_parts(url.to_owned(), url.to_owned(), 1, 1, now_secs, false);
            self.entries.insert(url.to_owned(), entry);
            self.prune_if_needed();
        }
    }

    fn update_title(&mut self, url: &str, title: &str) {
        if let Some(existing) = self.entries.get_mut(url) {
            if !title.trim().is_empty() {
                existing.title = title.to_owned();
            }
        }
    }

    fn set_bookmarked(&mut self, url: &str, title: &str, bookmarked: bool, now_secs: u64) {
        if let Some(existing) = self.entries.get_mut(url) {
            existing.is_bookmarked = bookmarked;
            if bookmarked && !title.trim().is_empty() {
                existing.title = title.to_owned();
            }
            if !bookmarked && existing.visit_count == 0 && existing.typed_count == 0 {
                self.entries.remove(url);
            }
        } else if bookmarked {
            let entry = CachedUrlEntry::from_parts(
                url.to_owned(),
                if title.trim().is_empty() {
                    url.to_owned()
                } else {
                    title.to_owned()
                },
                0,
                0,
                now_secs,
                true,
            );
            self.entries.insert(url.to_owned(), entry);
            self.prune_if_needed();
        }
    }

    fn remove_history_url(&mut self, url: &str) {
        if let Some(existing) = self.entries.get_mut(url) {
            if existing.is_bookmarked {
                existing.visit_count = 0;
                existing.typed_count = 0;
                existing.last_visited_at = 0;
            } else {
                self.entries.remove(url);
            }
        }
    }

    fn clear_history(&mut self) {
        self.entries.retain(|_, entry| {
            if entry.is_bookmarked {
                entry.visit_count = 0;
                entry.typed_count = 0;
                entry.last_visited_at = 0;
                true
            } else {
                false
            }
        });
    }

    fn prune_if_needed(&mut self) {
        if self.entries.len() <= OMNIBOX_CACHE_MAX_ENTRIES {
            return;
        }
        let now = unix_timestamp_secs();
        let mut ranked: Vec<(String, f64)> = self
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), score_cached_entry(v, now)))
            .collect();
        ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let remove_count = self.entries.len().saturating_sub(OMNIBOX_CACHE_MAX_ENTRIES);
        for (url, _) in ranked.into_iter().take(remove_count) {
            if self.entries.get(&url).is_some_and(|e| !e.is_bookmarked) {
                self.entries.remove(&url);
            }
        }
    }
}

type SuggestReply = Box<dyn FnOnce(u64, Vec<OmniboxSuggestion>) + Send>;

enum DbJob {
    WarmCache,
    RecordVisit {
        url: String,
        title: String,
        now: i64,
    },
    MarkTyped {
        url: String,
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
    /// Deletes every history row. Runs on the worker so it is ordered after any
    /// previously enqueued `RecordVisit` / `MarkTyped` jobs.
    ClearHistory(mpsc::Sender<()>),
    /// Deletes a single history row, ordered after previously enqueued writes.
    DeleteHistoryUrl {
        url: String,
        done: mpsc::Sender<()>,
    },
    Flush(mpsc::Sender<()>),
}

/// Online suggestion request handed from the DB worker to the dedicated network thread,
/// so slow suggestion endpoints never block history writes.
struct OnlineSuggestJob {
    query_id: u64,
    query: String,
    room: usize,
    search_engine: SearchEngine,
    local: Vec<OmniboxSuggestion>,
    reply: SuggestReply,
}

/// How long a synchronous caller waits for the DB worker to apply a queued job.
const WORKER_SYNC_TIMEOUT: Duration = Duration::from_secs(2);

/// Merges online search phrases into `suggestions`, skipping duplicates of existing rows.
fn merge_online_suggestions(
    suggestions: &mut Vec<OmniboxSuggestion>,
    online: Vec<String>,
    search_engine: SearchEngine,
) {
    for phrase in online {
        let search_url = search_engine.build_search_url(&phrase);
        if !suggestions
            .iter()
            .any(|s| s.title.eq_ignore_ascii_case(&phrase) || s.url == search_url)
        {
            suggestions.push(OmniboxSuggestion {
                kind: SuggestionKind::Search,
                title: phrase,
                url: search_url,
                score: 10.0,
                is_default: false,
            });
        }
    }
}

/// Runs on the `ember-suggest` thread: fetches online suggestions for the newest pending
/// request, dropping superseded ones without touching the network.
fn run_online_suggest_loop<F>(
    rx: mpsc::Receiver<OnlineSuggestJob>,
    latest_q: Arc<AtomicU64>,
    fetch: F,
) where
    F: Fn(&str, SearchEngine, usize) -> Vec<String>,
{
    while let Ok(mut job) = rx.recv() {
        // Collapse any backlog down to the most recent request.
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        if job.query_id < latest_q.load(Ordering::Relaxed) {
            continue;
        }
        let online = fetch(&job.query, job.search_engine, job.room);
        if job.query_id < latest_q.load(Ordering::Relaxed) {
            continue;
        }
        let mut suggestions = job.local;
        merge_online_suggestions(&mut suggestions, online, job.search_engine);
        (job.reply)(job.query_id, suggestions);
    }
}

/// Persistent and default browser configuration and SQLite storage.
#[derive(Debug, Clone)]
pub struct Storage {
    webview_data_dir: PathBuf,
    filters_dir: PathBuf,
    downloads_dir: PathBuf,
    logs_dir: PathBuf,
    db: Arc<Mutex<Option<Connection>>>,
    worker_tx: mpsc::Sender<DbJob>,
    latest_query_id: Arc<AtomicU64>,
    omnibox_cache: Arc<RwLock<OmniboxCache>>,
}

impl Storage {
    /// Initializes browser storage, ensures per-user directories exist, and opens `%LOCALAPPDATA%\Ember\ember.db`.
    pub fn init() -> io::Result<Self> {
        let base_dir = platform::resolve_app_data_dir()?;
        Self::with_base_dir(base_dir)
    }

    /// Ensures the default per-user `logs` directory exists and returns its path.
    pub fn ensure_default_logs_dir() -> io::Result<PathBuf> {
        let base_dir = platform::resolve_app_data_dir()?;
        let logs_dir = base_dir.join("logs");
        fs::create_dir_all(&logs_dir)?;
        Ok(logs_dir)
    }

    /// Initializes storage rooted at a specific directory (used by `init` and unit tests).
    pub fn with_base_dir(base_dir: PathBuf) -> io::Result<Self> {
        let webview_data_dir = base_dir.join("webview-data");
        let filters_dir = base_dir.join("filters");
        let downloads_dir =
            platform::resolve_downloads_dir().unwrap_or_else(|_| base_dir.join("Downloads"));
        let logs_dir = base_dir.join("logs");
        fs::create_dir_all(&webview_data_dir)?;
        fs::create_dir_all(&filters_dir)?;
        let _ = fs::create_dir_all(&downloads_dir);
        fs::create_dir_all(&logs_dir)?;

        let db_path = base_dir.join("ember.db");
        let conn = Connection::open(&db_path).ok();
        if let Some(ref c) = conn {
            Self::init_schema(c);
        }

        let db = Arc::new(Mutex::new(conn));
        let latest_query_id = Arc::new(AtomicU64::new(0));
        let omnibox_cache = Arc::new(RwLock::new(OmniboxCache::default()));
        let (worker_tx, worker_rx) = mpsc::channel::<DbJob>();

        // Dedicated network thread for online suggestions (exits when the DB worker exits).
        let (online_tx, online_rx) = mpsc::channel::<OnlineSuggestJob>();
        let online_latest_q = Arc::clone(&latest_query_id);
        let online_thread_ok = thread::Builder::new()
            .name(String::from("ember-suggest"))
            .spawn(move || {
                run_online_suggest_loop(
                    online_rx,
                    online_latest_q,
                    fetch_online_search_suggestions,
                );
            })
            .is_ok();

        let worker_db = Arc::clone(&db);
        let worker_latest_q = Arc::clone(&latest_query_id);
        let worker_cache = Arc::clone(&omnibox_cache);
        let _ = thread::Builder::new()
            .name(String::from("ember-db-worker"))
            .spawn(move || {
                while let Ok(job) = worker_rx.recv() {
                    match job {
                        DbJob::WarmCache => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    if let Ok(mut cache) = worker_cache.write() {
                                        cache.warm_from_db(c);
                                    }
                                }
                            }
                        }
                        DbJob::RecordVisit { url, title, now } => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_record_visit(c, &url, &title, now);
                                }
                            }
                        }
                        DbJob::MarkTyped { url, now } => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_mark_typed(c, &url, now);
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
                            let suggestions = rank_suggestions(
                                &query,
                                &history,
                                &bookmarks,
                                unix_timestamp_secs(),
                                limit,
                            );
                            if online_enabled && online_thread_ok && suggestions.len() < limit {
                                let trimmed = query.trim();
                                let lower = trimmed.to_ascii_lowercase();
                                let is_url_like = lower.starts_with("http://")
                                    || lower.starts_with("https://")
                                    || lower.starts_with("ember://");
                                if !is_url_like && trimmed.chars().count() >= 2 {
                                    let room = limit.saturating_sub(suggestions.len()).min(4);
                                    // Hand the network fetch to the suggestion thread so the DB
                                    // worker stays free for history writes.
                                    let job = OnlineSuggestJob {
                                        query_id,
                                        query: trimmed.to_owned(),
                                        room,
                                        search_engine,
                                        local: suggestions,
                                        reply,
                                    };
                                    if let Err(mpsc::SendError(job)) = online_tx.send(job) {
                                        (job.reply)(job.query_id, job.local);
                                    }
                                    continue;
                                }
                            }
                            reply(query_id, suggestions);
                        }
                        DbJob::ClearHistory(done) => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_clear_history(c);
                                }
                            }
                            let _ = done.send(());
                        }
                        DbJob::DeleteHistoryUrl { url, done } => {
                            if let Ok(guard) = worker_db.lock() {
                                if let Some(ref c) = *guard {
                                    execute_delete_history_url(c, &url);
                                }
                            }
                            let _ = done.send(());
                        }
                        DbJob::Flush(tx) => {
                            let _ = tx.send(());
                        }
                    }
                }
            });

        let _ = worker_tx.send(DbJob::WarmCache);

        Ok(Self {
            webview_data_dir,
            filters_dir,
            downloads_dir,
            logs_dir,
            db,
            worker_tx,
            latest_query_id,
            omnibox_cache,
        })
    }

    fn init_schema(conn: &Connection) {
        let _ = conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                visit_count INTEGER NOT NULL DEFAULT 1,
                typed_count INTEGER NOT NULL DEFAULT 0,
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
        // Migrate existing databases created before `typed_count` existed.
        let _ = conn.execute(
            "ALTER TABLE history ADD COLUMN typed_count INTEGER NOT NULL DEFAULT 0",
            [],
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

    /// Returns the directory where browser and UI logs are written (`%LOCALAPPDATA%\Ember\logs`).
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Appends a UI or popup JavaScript error line to `<logs_dir>/ui_errors.log`.
    pub fn log_ui_js_error(&self, source: &str, detail: &str) {
        Self::append_ui_error_to_dir(&self.logs_dir, source, detail);
    }

    /// Appends a formatted error entry to `ui_errors.log` inside `logs_dir`.
    pub fn append_ui_error_to_dir(logs_dir: &Path, source: &str, detail: &str) {
        use std::io::Write as _;

        if fs::create_dir_all(logs_dir).is_err() {
            return;
        }
        let log_path = logs_dir.join("ui_errors.log");
        let sanitized = detail.replace(['\r', '\n'], " ");
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(
                file,
                "[ts={}] [{}] {}",
                unix_timestamp_secs(),
                source,
                sanitized
            );
        }
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
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.upsert_visit(trimmed, clean_title, now.max(0) as u64, false);
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
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.upsert_visit(trimmed, clean_title, now.max(0) as u64, false);
        }
        let _ = self.worker_tx.send(DbJob::RecordVisit {
            url: trimmed.to_owned(),
            title: clean_title.to_owned(),
            now,
        });
    }

    /// Marks `url` as having been explicitly typed/selected in the omnibox so it scores higher for autofill.
    pub fn mark_url_typed_async(&self, url: &str) {
        let trimmed = url.trim();
        if !should_track_url(trimmed) {
            return;
        }
        let now = unix_timestamp_secs() as i64;
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.mark_typed(trimmed, now.max(0) as u64);
        }
        let _ = self.worker_tx.send(DbJob::MarkTyped {
            url: trimmed.to_owned(),
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
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.update_title(trimmed_url, trimmed_title);
        }
    }

    /// Enqueues a non-blocking title update onto the background DB worker thread.
    pub fn update_visit_title_async(&self, url: &str, title: &str) {
        let trimmed_url = url.trim();
        let trimmed_title = title.trim();
        if !should_track_url(trimmed_url) || trimmed_title.is_empty() {
            return;
        }
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.update_title(trimmed_url, trimmed_title);
        }
        let _ = self.worker_tx.send(DbJob::UpdateVisitTitle {
            url: trimmed_url.to_owned(),
            title: trimmed_title.to_owned(),
        });
    }

    /// Answers inline autofill and local suggestions instantaneously from the in-memory cache
    /// without touching SQLite on the keystroke path.
    pub fn query_omnibox_cached(
        &self,
        query: &str,
        allow_autofill: bool,
        limit: usize,
    ) -> (Option<AutofillMatch>, Vec<OmniboxSuggestion>) {
        let now_secs = unix_timestamp_secs();
        let cache = match self.omnibox_cache.read() {
            Ok(guard) => guard,
            Err(_) => return (None, Vec::new()),
        };
        query_cached_entries(query, allow_autofill, &cache.entries, now_secs, limit)
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

    /// Invalidates any in-flight background `suggest_async` queries so stale results are dropped.
    pub fn cancel_pending_queries(&self) {
        self.latest_query_id.fetch_add(1, Ordering::Relaxed);
    }

    /// Blocks until all previously enqueued background DB jobs have completed.
    pub fn flush_worker(&self) {
        let (tx, rx) = mpsc::channel();
        if self.worker_tx.send(DbJob::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }

    /// Test helper: blocks (without a timeout) until every previously queued job has run, so
    /// tests stay deterministic on heavily loaded machines.
    #[cfg(test)]
    fn flush_worker_blocking(&self) {
        let (tx, rx) = mpsc::channel();
        if self.worker_tx.send(DbJob::Flush(tx)).is_ok() {
            let _ = rx.recv();
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

    /// Deletes a single URL from browsing history and the in-memory omnibox cache.
    ///
    /// The delete is routed through the background DB worker so it is strictly ordered after any
    /// `record_visit_async` / `mark_url_typed_async` jobs already queued for the same URL (which
    /// would otherwise re-insert the row). Blocks until the worker has applied it (bounded by a
    /// timeout); falls back to a direct delete if the worker is gone.
    pub fn delete_history_url(&self, url: &str) {
        let trimmed = url.trim();
        // Update the cache first: every earlier async write already touched it, so the cache
        // reflects the same ordering as the worker queue.
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.remove_history_url(trimmed);
        }
        let (done_tx, done_rx) = mpsc::channel();
        let job = DbJob::DeleteHistoryUrl {
            url: trimmed.to_owned(),
            done: done_tx,
        };
        if self.worker_tx.send(job).is_ok() {
            let _ = done_rx.recv_timeout(WORKER_SYNC_TIMEOUT);
        } else if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                execute_delete_history_url(conn, trimmed);
            }
        }
    }

    /// Clears all browsing history from SQLite and the in-memory omnibox cache.
    ///
    /// Like [`Storage::delete_history_url`], the delete is queued on the DB worker behind any
    /// pending async history writes so cleared entries cannot reappear afterwards.
    pub fn clear_history(&self) {
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.clear_history();
        }
        let (done_tx, done_rx) = mpsc::channel();
        if self.worker_tx.send(DbJob::ClearHistory(done_tx)).is_ok() {
            let _ = done_rx.recv_timeout(WORKER_SYNC_TIMEOUT);
        } else if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                execute_clear_history(conn);
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
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.set_bookmarked(trimmed, clean_title, true, now.max(0) as u64);
        }
    }

    /// Removes a bookmark by URL.
    pub fn remove_bookmark(&self, url: &str) {
        let trimmed = url.trim();
        if let Ok(guard) = self.db.lock() {
            if let Some(ref conn) = *guard {
                let _ = conn.execute("DELETE FROM bookmarks WHERE url = ?1", params![trimmed]);
            }
        }
        if let Ok(mut cache) = self.omnibox_cache.write() {
            cache.set_bookmarked(trimmed, "", false, 0);
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

fn execute_clear_history(conn: &Connection) {
    let _ = conn.execute("DELETE FROM history", []);
}

fn execute_delete_history_url(conn: &Connection, url: &str) {
    let _ = conn.execute("DELETE FROM history WHERE url = ?1", params![url]);
}

/// Bumps only `typed_count` for an existing row (the visit itself is recorded separately by
/// `execute_record_visit`); inserts a fresh row with `visit_count = 1` only if the URL is missing.
fn execute_mark_typed(conn: &Connection, url: &str, now: i64) {
    let _ = conn.execute(
        "INSERT INTO history (url, title, visit_count, typed_count, last_visited_at)
         VALUES (?1, ?1, 1, 1, ?2)
         ON CONFLICT(url) DO UPDATE SET
            typed_count = history.typed_count + 1,
            last_visited_at = MAX(history.last_visited_at, excluded.last_visited_at)",
        params![url, now],
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

fn strip_url_scheme(url_lower: &str) -> &str {
    url_lower
        .strip_prefix("https://")
        .or_else(|| url_lower.strip_prefix("http://"))
        .unwrap_or(url_lower)
}

fn extract_host_from_stripped(stripped: &str) -> &str {
    stripped.split(['/', '?', '#']).next().unwrap_or(stripped)
}

fn host_level_url(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    let scheme_len = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        0
    };
    let rest = &url[scheme_len..];
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    format!("{}{}/", &url[..scheme_len], &rest[..host_end])
}

fn score_cached_entry(entry: &CachedUrlEntry, now_secs: u64) -> f64 {
    let visit_boost = (entry.visit_count.max(0) as f64).ln_1p() * 90.0;
    let typed_bonus = if entry.typed_count > 0 {
        260.0 + (entry.typed_count as f64).ln_1p() * 120.0
    } else {
        0.0
    };
    let bookmark_bonus = if entry.is_bookmarked { 240.0 } else { 0.0 };
    let recency_boost = if entry.last_visited_at > 0 {
        let age_hours = now_secs.saturating_sub(entry.last_visited_at) as f64 / 3600.0;
        150.0 / (1.0 + age_hours / 24.0)
    } else {
        0.0
    };
    visit_boost + typed_bonus + bookmark_bonus + recency_boost
}

/// Pure inline autofill matcher against pre-normalized cached URL entries.
///
/// Matches host-first (ignoring `http(s)://` and leading `www.`), then full `host + path` once `/`
/// is typed, and also supports explicit `http://`, `https://`, or `www.` prefixes.
pub fn find_inline_autofill<'a, I>(
    query: &str,
    allow_autofill: bool,
    entries: I,
    now_secs: u64,
) -> Option<AutofillMatch>
where
    I: IntoIterator<Item = &'a CachedUrlEntry>,
{
    if !allow_autofill
        || query.is_empty()
        || query.contains(' ')
        || query.chars().any(char::is_whitespace)
    {
        return None;
    }

    let q_lower = query.to_ascii_lowercase();
    let has_scheme = q_lower.starts_with("http://") || q_lower.starts_with("https://");
    let has_www = !has_scheme && q_lower.starts_with("www.");

    let mut best: Option<AutofillMatch> = None;

    for entry in entries {
        let (completed_text, full_url, exact_host_bonus) = if has_scheme {
            if !entry.url_lower.starts_with(&q_lower) {
                continue;
            }
            let after_scheme = q_lower.find("://").map(|i| i + 3).unwrap_or(0);
            let typed_has_path = q_lower[after_scheme..].contains('/');
            if typed_has_path {
                let suffix = &entry.url_lower[q_lower.len()..];
                (format!("{query}{suffix}"), entry.url.clone(), 0.0)
            } else {
                let url_after_scheme = &entry.url_lower[after_scheme..];
                let host = extract_host_from_stripped(url_after_scheme);
                let scheme = &q_lower[..after_scheme];
                let target_prefix = format!("{scheme}{host}");
                if !target_prefix.starts_with(&q_lower) {
                    continue;
                }
                let suffix = &target_prefix[q_lower.len()..];
                let is_root = url_after_scheme.trim_end_matches('/') == host;
                (
                    format!("{query}{suffix}"),
                    host_level_url(&entry.url),
                    if is_root { 40.0 } else { 0.0 },
                )
            }
        } else if has_www {
            let no_scheme = strip_url_scheme(&entry.url_lower);
            if !no_scheme.starts_with(&q_lower) {
                continue;
            }
            if q_lower.contains('/') {
                let suffix = &no_scheme[q_lower.len()..];
                (format!("{query}{suffix}"), entry.url.clone(), 0.0)
            } else {
                let host = extract_host_from_stripped(no_scheme);
                if !host.starts_with(&q_lower) {
                    continue;
                }
                let suffix = &host[q_lower.len()..];
                let is_root = no_scheme.trim_end_matches('/') == host;
                (
                    format!("{query}{suffix}"),
                    host_level_url(&entry.url),
                    if is_root { 40.0 } else { 0.0 },
                )
            }
        } else if q_lower.contains('/') {
            if !entry.host_path_no_www.starts_with(&q_lower) {
                continue;
            }
            let suffix = &entry.host_path_no_www[q_lower.len()..];
            (format!("{query}{suffix}"), entry.url.clone(), 0.0)
        } else {
            if !entry.host_no_www.starts_with(&q_lower) {
                continue;
            }
            let suffix = &entry.host_no_www[q_lower.len()..];
            let is_root = entry.host_path_no_www.trim_end_matches('/') == entry.host_no_www;
            (
                format!("{query}{suffix}"),
                host_level_url(&entry.url),
                if is_root { 40.0 } else { 0.0 },
            )
        };

        let score = score_cached_entry(entry, now_secs) + exact_host_bonus;
        let kind = if entry.is_bookmarked {
            SuggestionKind::Bookmark
        } else {
            SuggestionKind::History
        };
        let title = if entry.title.trim().is_empty() {
            full_url.clone()
        } else {
            entry.title.clone()
        };

        let replace = match best {
            None => true,
            Some(ref cur) => {
                score > cur.score
                    || ((score - cur.score).abs() < f64::EPSILON
                        && completed_text < cur.completed_text)
            }
        };
        if replace {
            best = Some(AutofillMatch {
                completed_text,
                full_url,
                title,
                kind,
                score,
            });
        }
    }

    best
}

fn query_cached_entries(
    query: &str,
    allow_autofill: bool,
    entries: &HashMap<String, CachedUrlEntry>,
    now_secs: u64,
    limit: usize,
) -> (Option<AutofillMatch>, Vec<OmniboxSuggestion>) {
    let autofill = find_inline_autofill(query, allow_autofill, entries.values(), now_secs);
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() || limit == 0 {
        return (autofill, Vec::new());
    }

    let mut suggestions: Vec<OmniboxSuggestion> = Vec::new();
    for entry in entries.values() {
        let title_lower = entry.title.to_ascii_lowercase();
        let match_score = if entry.host_path_no_www.starts_with(&q) {
            1000.0
        } else if entry.url_lower.starts_with(&q) {
            950.0
        } else if title_lower.starts_with(&q) {
            700.0
        } else if entry.host_path_no_www.contains(&q) {
            350.0
        } else if title_lower.contains(&q) {
            250.0
        } else {
            continue;
        };

        let score = match_score + score_cached_entry(entry, now_secs);
        suggestions.push(OmniboxSuggestion {
            kind: if entry.is_bookmarked {
                SuggestionKind::Bookmark
            } else {
                SuggestionKind::History
            },
            title: if entry.title.trim().is_empty() {
                entry.url.clone()
            } else {
                entry.title.clone()
            },
            url: entry.url.clone(),
            score,
            is_default: false,
        });
    }

    suggestions.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.url.cmp(&b.url))
    });

    if let Some(ref af) = autofill {
        if let Some(pos) = suggestions.iter().position(|s| s.url == af.full_url) {
            let mut item = suggestions.remove(pos);
            item.is_default = true;
            suggestions.insert(0, item);
        } else {
            suggestions.insert(
                0,
                OmniboxSuggestion {
                    kind: af.kind,
                    title: af.title.clone(),
                    url: af.full_url.clone(),
                    score: af.score + 2000.0,
                    is_default: true,
                },
            );
        }
    }

    suggestions.truncate(limit);
    (autofill, suggestions)
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
                is_default: false,
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
                    is_default: false,
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
        fn InternetSetOptionW(
            h_internet: isize,
            dw_option: u32,
            lp_buffer: *const core::ffi::c_void,
            dw_buffer_length: u32,
        ) -> i32;
    }

    /// Per-operation timeout for online suggestion requests.
    const HTTP_TIMEOUT_MS: u32 = 3000;

    /// Applies connect/send/receive timeouts to a WinINet session handle so a slow or
    /// unresponsive suggestion endpoint cannot stall the calling thread indefinitely.
    fn apply_timeouts(h_inet: isize) {
        const INTERNET_OPTION_CONNECT_TIMEOUT: u32 = 2;
        const INTERNET_OPTION_SEND_TIMEOUT: u32 = 5;
        const INTERNET_OPTION_RECEIVE_TIMEOUT: u32 = 6;
        const INTERNET_OPTION_DATA_SEND_TIMEOUT: u32 = 7;
        const INTERNET_OPTION_DATA_RECEIVE_TIMEOUT: u32 = 8;
        let timeout: u32 = HTTP_TIMEOUT_MS;
        for option in [
            INTERNET_OPTION_CONNECT_TIMEOUT,
            INTERNET_OPTION_SEND_TIMEOUT,
            INTERNET_OPTION_RECEIVE_TIMEOUT,
            INTERNET_OPTION_DATA_SEND_TIMEOUT,
            INTERNET_OPTION_DATA_RECEIVE_TIMEOUT,
        ] {
            // SAFETY: `timeout` is a live u32 for the duration of the call and the length matches.
            unsafe {
                InternetSetOptionW(
                    h_inet,
                    option,
                    (&timeout as *const u32).cast(),
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
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
        apply_timeouts(h_inet);
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

        storage.update_visit_title("https://docs.rs", "Docs.rs Ã¢â‚¬â€ Rust Crate Docs");
        let updated = storage.recent_history(10);
        assert!(updated
            .iter()
            .any(|e| e.url == "https://docs.rs" && e.title == "Docs.rs Ã¢â‚¬â€ Rust Crate Docs"));

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
        storage.flush_worker_blocking();

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

    #[test]
    fn test_inline_autofill_scheme_and_www_stripping() {
        let now = 1_700_000_000;
        let entries = vec![
            CachedUrlEntry::from_parts(
                String::from("https://www.facebook.com/"),
                String::from("Facebook"),
                5,
                0,
                now,
                false,
            ),
            CachedUrlEntry::from_parts(
                String::from("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
                String::from("Video"),
                3,
                0,
                now,
                false,
            ),
        ];

        let fb = find_inline_autofill("face", true, &entries, now).expect("facebook match");
        assert_eq!(fb.completed_text, "facebook.com");
        assert_eq!(fb.full_url, "https://www.facebook.com/");

        let yt = find_inline_autofill("yout", true, &entries, now).expect("youtube match");
        assert_eq!(yt.completed_text, "youtube.com");
        assert_eq!(yt.full_url, "https://www.youtube.com/");
    }

    #[test]
    fn test_inline_autofill_scoring_typed_and_bookmarked_beat_plain() {
        let now = 1_700_000_000;
        let entries = vec![
            CachedUrlEntry::from_parts(
                String::from("https://rust-analyzer.github.io/"),
                String::from("rust-analyzer"),
                10,
                0,
                now,
                false,
            ),
            CachedUrlEntry::from_parts(
                String::from("https://rust-lang.org/"),
                String::from("Rust Programming Language"),
                3,
                1,
                now,
                false,
            ),
            CachedUrlEntry::from_parts(
                String::from("https://rustacean.net/"),
                String::from("Rustacean"),
                2,
                0,
                now,
                true,
            ),
        ];

        // Typed entry beats plain higher-visit entry
        let typed_win = find_inline_autofill("rust-", true, &entries, now).expect("match");
        assert_eq!(typed_win.completed_text, "rust-lang.org");

        // Bookmarked entry beats plain higher-visit entry
        let bm_entries = vec![entries[0].clone(), entries[2].clone()];
        let bm_win = find_inline_autofill("rust", true, &bm_entries, now).expect("bm match");
        assert_eq!(bm_win.completed_text, "rustacean.net");
    }

    #[test]
    fn test_inline_autofill_suppressed_on_delete_and_spaces() {
        let now = 1_700_000_000;
        let entries = vec![CachedUrlEntry::from_parts(
            String::from("https://youtube.com/"),
            String::from("YouTube"),
            10,
            2,
            now,
            true,
        )];

        assert!(find_inline_autofill("yout", false, &entries, now).is_none());
        assert!(find_inline_autofill("you tube", true, &entries, now).is_none());
        assert!(find_inline_autofill("", true, &entries, now).is_none());
    }

    #[test]
    fn test_inline_autofill_url_path_matching() {
        let now = 1_700_000_000;
        let entries = vec![
            CachedUrlEntry::from_parts(
                String::from("https://docs.rs/tokio/latest/tokio/"),
                String::from("tokio - Rust"),
                8,
                1,
                now,
                false,
            ),
            CachedUrlEntry::from_parts(
                String::from("https://docs.rs/serde/latest/serde/"),
                String::from("serde - Rust"),
                4,
                0,
                now,
                false,
            ),
        ];

        // Host-first before '/'
        let host_match = find_inline_autofill("docs", true, &entries, now).expect("host");
        assert_eq!(host_match.completed_text, "docs.rs");

        // Full path matching once '/' is typed
        let path_match = find_inline_autofill("docs.rs/tok", true, &entries, now).expect("path");
        assert_eq!(path_match.completed_text, "docs.rs/tokio/latest/tokio/");
        assert_eq!(path_match.full_url, "https://docs.rs/tokio/latest/tokio/");
    }

    #[test]
    fn test_omnibox_cache_clear_history_and_default_row() {
        let storage = temp_storage("autofill_cache");
        storage.record_visit_async("https://www.github.com/torvalds/linux", "Linux");
        storage.mark_url_typed_async("https://www.github.com/torvalds/linux");
        storage.flush_worker_blocking();

        let (af, suggestions) = storage.query_omnibox_cached("gith", true, 8);
        let af = af.expect("cached autofill");
        assert_eq!(af.completed_text, "github.com");
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].is_default);
        assert_eq!(suggestions[0].url, af.full_url);

        // Clearing history must clear the in-memory cache immediately.
        storage.clear_history();
        let (af_after, suggestions_after) = storage.query_omnibox_cached("gith", true, 8);
        assert!(af_after.is_none());
        assert!(suggestions_after.is_empty());

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    #[test]
    fn test_inline_autofill_five_visited_sites() {
        let storage = temp_storage("five_sites_autofill");
        let sites = [
            ("https://www.youtube.com/watch?v=abc", "YouTube"),
            ("https://www.facebook.com/", "Facebook"),
            ("https://github.com/rust-lang/rust", "Rust GitHub"),
            ("https://docs.rs/tokio", "tokio - Docs"),
            ("https://news.ycombinator.com/", "Hacker News"),
        ];
        for (url, title) in sites {
            storage.record_visit_async(url, title);
        }
        storage.flush_worker_blocking();

        let expected = [
            ("yout", "youtube.com", "https://www.youtube.com/"),
            ("face", "facebook.com", "https://www.facebook.com/"),
            ("gith", "github.com", "https://github.com/"),
            ("docs", "docs.rs", "https://docs.rs/"),
            (
                "news",
                "news.ycombinator.com",
                "https://news.ycombinator.com/",
            ),
        ];
        for (prefix, completed, full_url) in expected {
            let (af, suggestions) = storage.query_omnibox_cached(prefix, true, 8);
            let af = af.expect("autofill match");
            assert_eq!(af.completed_text, completed);
            assert_eq!(af.full_url, full_url);
            assert!(suggestions[0].is_default);
            assert_eq!(suggestions[0].url, full_url);

            // Simulates Backspace/Delete removing suffix (`allow_autofill = false`)
            let (af_del, _) = storage.query_omnibox_cached(prefix, false, 8);
            assert!(af_del.is_none());
        }

        // Path matching after '/'
        let (path_af, _) = storage.query_omnibox_cached("github.com/ru", true, 8);
        let path_af = path_af.expect("path autofill");
        assert_eq!(path_af.completed_text, "github.com/rust-lang/rust");

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    fn db_counts(storage: &Storage, url: &str) -> Option<(i64, i64)> {
        let guard = storage.db.lock().expect("db lock");
        let conn = guard.as_ref().expect("db open");
        conn.query_row(
            "SELECT visit_count, typed_count FROM history WHERE url = ?1",
            params![url],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .ok()
    }

    fn cache_counts(storage: &Storage, url: &str) -> Option<(i64, i64)> {
        let cache = storage.omnibox_cache.read().expect("cache lock");
        cache
            .entries
            .get(url)
            .map(|e| (e.visit_count, e.typed_count))
    }

    #[test]
    fn test_clear_history_is_ordered_after_pending_async_writes() {
        let storage = temp_storage("clear_after_async");
        // Enqueue many async writes and clear immediately, without flushing first.
        // Kept small so the queued SQLite writes finish well within the flush timeout.
        for i in 0..20 {
            let url = format!("https://example{}.com/page", i % 7);
            storage.record_visit_async(&url, "Example");
            storage.mark_url_typed_async(&url);
        }
        storage.clear_history();
        storage.flush_worker_blocking();

        assert!(storage.recent_history(100).is_empty());
        assert!(storage.most_visited(100).is_empty());
        let (af, suggestions) = storage.query_omnibox_cached("example", true, 8);
        assert!(af.is_none());
        assert!(suggestions.is_empty());

        // Visits recorded after the clear are kept.
        storage.record_visit_async("https://after.example.org/", "After");
        storage.flush_worker_blocking();
        assert_eq!(storage.recent_history(10).len(), 1);

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    #[test]
    fn test_delete_history_url_is_ordered_after_pending_async_writes() {
        let storage = temp_storage("delete_after_async");
        storage.record_visit_async("https://keep.example.com/", "Keep");
        for _ in 0..10 {
            storage.record_visit_async("https://gone.example.com/", "Gone");
            storage.mark_url_typed_async("https://gone.example.com/");
        }
        storage.delete_history_url("https://gone.example.com/");
        storage.flush_worker_blocking();

        let entries = storage.recent_history(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://keep.example.com/");
        assert!(cache_counts(&storage, "https://gone.example.com/").is_none());

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    #[test]
    fn test_mark_typed_after_visit_only_bumps_typed_count() {
        let storage = temp_storage("mark_typed_counts");
        let url = "https://typed.example.com/";

        storage.record_visit_async(url, "Typed");
        storage.mark_url_typed_async(url);
        storage.flush_worker_blocking();
        assert_eq!(db_counts(&storage, url), Some((1, 1)));
        assert_eq!(cache_counts(&storage, url), Some((1, 1)));

        // Missing row: mark_typed inserts with visit_count 1.
        let fresh = "https://fresh.example.com/";
        storage.mark_url_typed_async(fresh);
        storage.flush_worker_blocking();
        assert_eq!(db_counts(&storage, fresh), Some((1, 1)));
        assert_eq!(cache_counts(&storage, fresh), Some((1, 1)));

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }

    #[test]
    fn test_online_suggest_loop_skips_stale_and_merges() {
        let latest = Arc::new(AtomicU64::new(0));
        let (job_tx, job_rx) = mpsc::channel::<OnlineSuggestJob>();
        let (reply_tx, reply_rx) = mpsc::channel::<(u64, Vec<OmniboxSuggestion>)>();

        let local = vec![OmniboxSuggestion {
            kind: SuggestionKind::History,
            title: String::from("rust lang"),
            url: String::from("https://rust-lang.org/"),
            score: 1000.0,
            is_default: false,
        }];
        for (id, query) in [(1u64, "ru"), (2u64, "rus")] {
            let tx = reply_tx.clone();
            job_tx
                .send(OnlineSuggestJob {
                    query_id: id,
                    query: query.to_owned(),
                    room: 4,
                    search_engine: SearchEngine::DuckDuckGo,
                    local: local.clone(),
                    reply: Box::new(move |qid, items| {
                        let _ = tx.send((qid, items));
                    }),
                })
                .expect("queue job");
        }
        latest.store(2, Ordering::Relaxed);
        drop(job_tx);
        drop(reply_tx);

        let fetch_calls = Arc::new(AtomicU64::new(0));
        let calls = Arc::clone(&fetch_calls);
        run_online_suggest_loop(job_rx, Arc::clone(&latest), move |q, _engine, _room| {
            calls.fetch_add(1, Ordering::Relaxed);
            vec![format!("{q}t"), String::from("RUST LANG")]
        });

        let replies: Vec<_> = reply_rx.iter().collect();
        assert_eq!(replies.len(), 1, "stale query 1 must be dropped");
        assert_eq!(fetch_calls.load(Ordering::Relaxed), 1);
        let (qid, items) = &replies[0];
        assert_eq!(*qid, 2);
        // Local row kept, duplicate phrase (case-insensitive) skipped, new phrase appended.
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].title, "rust");
        assert_eq!(items[1].kind, SuggestionKind::Search);
    }

    #[test]
    fn test_ui_js_error_log_written_to_logs_dir() {
        let storage = temp_storage("ui_error_log");
        storage.log_ui_js_error("ui", "ReferenceError: test is not defined\nline 42");
        storage.log_ui_js_error("popup", "Unhandled promise rejection");

        let log_path = storage.logs_dir().join("ui_errors.log");
        assert!(log_path.exists());
        let contents = fs::read_to_string(&log_path).expect("read ui_errors.log");
        assert!(contents.contains("[ui] ReferenceError: test is not defined line 42"));
        assert!(contents.contains("[popup] Unhandled promise rejection"));

        let _ = fs::remove_dir_all(storage.filters_dir().parent().unwrap());
    }
}
