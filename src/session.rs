//! Session restore: the open tabs of a profile are saved to `session.txt` in its config dir
//! (debounced after tab changes and on exit) and restored lazily at startup when "On startup"
//! is "Continue where you left off". Private windows and self-tests never save or restore.
//!
//! File format (UTF-8, one record per line):
//!
//! ```text
//! # Ember session v1
//! active=<index>
//! <url>\t<title>
//! ```

use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// File name of the saved session inside a profile's config dir.
pub const SESSION_FILE: &str = "session.txt";
/// Maximum number of tabs saved / restored.
pub const MAX_SESSION_TABS: usize = 200;
/// URLs longer than this are not saved (huge `data:`-like URLs, abuse).
const MAX_URL_LEN: usize = 8192;
/// Saved titles are truncated to this many characters.
const MAX_TITLE_CHARS: usize = 300;
/// Quiet period after the last tab change before the session is written.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// One saved tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionTab {
    pub url: String,
    pub title: String,
}

/// The saved tabs of a window and the index of the active one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active: usize,
}

/// Returns `true` if a tab showing `url` should be saved and restored: `http(s)` pages and
/// internal `ember://` pages, except the HTTPS-only warning page and the self-test page.
pub fn should_save_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_URL_LEN || url.chars().any(char::is_control) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("ember://") {
        let path = rest
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_matches('/');
        return path != "warning" && path != "selftest";
    }
    false
}

fn clean_title(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TITLE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

impl Session {
    /// Builds a session from `(url, title, is_active)` tuples, dropping tabs that must not be
    /// saved. If the active tab is dropped, the closest preceding saved tab becomes active.
    pub fn from_tabs<'a, I>(tabs: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str, bool)>,
    {
        let mut session = Self::default();
        let mut active_found = false;
        for (url, title, is_active) in tabs {
            if session.tabs.len() >= MAX_SESSION_TABS {
                break;
            }
            if should_save_url(url) {
                session.tabs.push(SessionTab {
                    url: url.trim().to_owned(),
                    title: clean_title(title),
                });
                if is_active {
                    session.active = session.tabs.len() - 1;
                    active_found = true;
                }
            } else if is_active && !active_found {
                session.active = session.tabs.len().saturating_sub(1);
            }
        }
        if session.active >= session.tabs.len() {
            session.active = 0;
        }
        session
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Serializes the session into the `session.txt` format.
    pub fn serialize(&self) -> String {
        let mut out = format!("# Ember session v1\nactive={}\n", self.active);
        for tab in &self.tabs {
            out.push_str(&tab.url);
            out.push('\t');
            out.push_str(&clean_title(&tab.title));
            out.push('\n');
        }
        out
    }

    /// Parses `session.txt`, ignoring malformed lines and unsafe URLs.
    pub fn parse(raw: &str) -> Self {
        let mut session = Self::default();
        let mut active = 0usize;
        for line in raw.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(idx) = line.strip_prefix("active=") {
                active = idx.trim().parse().unwrap_or(0);
                continue;
            }
            if session.tabs.len() >= MAX_SESSION_TABS {
                continue;
            }
            let (url, title) = line.split_once('\t').unwrap_or((line, ""));
            if should_save_url(url) {
                session.tabs.push(SessionTab {
                    url: url.trim().to_owned(),
                    title: clean_title(title),
                });
            }
        }
        session.active = if active < session.tabs.len() {
            active
        } else {
            0
        };
        session
    }
}

/// Loads the saved session from `config_dir`, if any (`None` when missing or empty).
pub fn load(config_dir: &Path) -> Option<Session> {
    let raw = fs::read_to_string(config_dir.join(SESSION_FILE)).ok()?;
    let session = Session::parse(&raw);
    (!session.is_empty()).then_some(session)
}

/// Writes `session` to `config_dir` atomically (temporary file + rename).
pub fn save(config_dir: &Path, session: &Session) -> io::Result<()> {
    let path = config_dir.join(SESSION_FILE);
    let tmp = config_dir.join("session.txt.tmp");
    fs::write(&tmp, session.serialize())?;
    fs::rename(&tmp, &path)
}

static TABS_CHANGED: AtomicBool = AtomicBool::new(false);

/// Signals that the tab list, a tab URL/title or the active tab may have changed. Called from
/// the chrome UI sync, which runs after every such change.
pub fn notify_tabs_changed() {
    TABS_CHANGED.store(true, Ordering::Relaxed);
}

fn take_tabs_changed() -> bool {
    TABS_CHANGED.swap(false, Ordering::Relaxed)
}

/// Debounces session writes and skips writes when nothing changed.
#[derive(Debug, Default)]
pub struct SessionSaver {
    dirty_since: Option<Instant>,
    last_written: Option<String>,
}

impl SessionSaver {
    /// Records a pending change at `now` (keeps the earliest unsaved change time so a stream
    /// of changes still saves at least every [`SAVE_DEBOUNCE`]).
    pub fn mark_dirty(&mut self, now: Instant) {
        if self.dirty_since.is_none() {
            self.dirty_since = Some(now);
        }
    }

    /// Picks up [`notify_tabs_changed`] signals and returns `true` when a save is due.
    pub fn poll(&mut self, now: Instant) -> bool {
        if take_tabs_changed() {
            self.mark_dirty(now);
        }
        self.dirty_since
            .is_some_and(|since| now.saturating_duration_since(since) >= SAVE_DEBOUNCE)
    }

    /// Writes `session` unless it equals the last written one. Clears the dirty state.
    pub fn save(&mut self, config_dir: &Path, session: &Session) {
        self.dirty_since = None;
        let text = session.serialize();
        if self.last_written.as_deref() == Some(text.as_str()) {
            return;
        }
        match save(config_dir, session) {
            Ok(()) => self.last_written = Some(text),
            Err(err) => eprintln!("[ember][session] failed to save session: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_save_url_filters() {
        assert!(should_save_url("https://example.com/a?b=1#c"));
        assert!(should_save_url("http://neverssl.com/"));
        assert!(should_save_url("ember://settings"));
        assert!(should_save_url("ember://newtab"));
        assert!(!should_save_url("ember://warning?url=http%3A%2F%2Fa.test"));
        assert!(!should_save_url("EMBER://Warning/"));
        assert!(!should_save_url("ember://selftest"));
        assert!(!should_save_url("about:blank"));
        assert!(!should_save_url("javascript:alert(1)"));
        assert!(!should_save_url("data:text/html,hi"));
        assert!(!should_save_url("file:///C:/secret.txt"));
        assert!(!should_save_url(""));
        assert!(!should_save_url("https://a.test/\nactive=5"));
        assert!(!should_save_url(&format!(
            "https://a.test/{}",
            "x".repeat(9000)
        )));
    }

    #[test]
    fn test_session_roundtrip() {
        let session = Session {
            tabs: vec![
                SessionTab {
                    url: String::from("https://example.com/"),
                    title: String::from("Example \t Domain\n"),
                },
                SessionTab {
                    url: String::from("ember://settings"),
                    title: String::from("Settings"),
                },
                SessionTab {
                    url: String::from("https://例え.jp/ページ"),
                    title: String::new(),
                },
            ],
            active: 1,
        };
        let parsed = Session::parse(&session.serialize());
        assert_eq!(parsed.active, 1);
        assert_eq!(parsed.tabs.len(), 3);
        assert_eq!(parsed.tabs[0].title, "Example   Domain");
        assert_eq!(parsed.tabs[1].url, "ember://settings");
        assert_eq!(parsed.tabs[2].url, "https://例え.jp/ページ");
        assert_eq!(Session::parse(&parsed.serialize()), parsed);
    }

    #[test]
    fn test_session_from_tabs_skips_warning_and_remaps_active() {
        let tabs = [
            ("https://a.test/", "A", false),
            ("ember://warning?url=http%3A%2F%2Fb.test", "Warning", true),
            ("https://c.test/", "C", false),
        ];
        let session = Session::from_tabs(tabs.iter().copied());
        assert_eq!(session.tabs.len(), 2);
        assert_eq!(session.active, 0, "falls back to the preceding saved tab");

        let session = Session::from_tabs(
            [("about:blank", "", true), ("https://x.test/", "X", false)]
                .iter()
                .copied(),
        );
        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.active, 0);

        let session = Session::from_tabs(
            [
                ("https://x.test/", "X", false),
                ("https://y.test/", "Y", true),
            ]
            .iter()
            .copied(),
        );
        assert_eq!(session.active, 1);
        assert!(Session::from_tabs(std::iter::empty()).is_empty());
    }

    #[test]
    fn test_session_parse_is_robust() {
        let raw = "garbage line without tab\nactive=99\n\tno url\nhttps://ok.test/\tOK\r\njavascript:x\tbad\n# comment\n";
        let session = Session::parse(raw);
        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.tabs[0].url, "https://ok.test/");
        assert_eq!(session.active, 0, "out-of-range active index is clamped");

        let many: String = (0..500)
            .map(|i| format!("https://t{i}.test/\tT\n"))
            .collect();
        assert_eq!(Session::parse(&many).tabs.len(), MAX_SESSION_TABS);
        assert!(Session::parse("").is_empty());
    }

    #[test]
    fn test_session_save_load_and_saver() {
        let dir = std::env::temp_dir().join(format!("ember_session_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");
        assert!(load(&dir).is_none());

        let session = Session::from_tabs(
            [
                ("https://a.test/", "A", false),
                ("https://b.test/", "B", true),
            ]
            .iter()
            .copied(),
        );
        let mut saver = SessionSaver::default();
        let t0 = Instant::now();
        saver.mark_dirty(t0);
        assert!(!saver.poll(t0));
        assert!(saver.poll(t0 + SAVE_DEBOUNCE));
        saver.save(&dir, &session);
        assert!(!saver.poll(t0 + SAVE_DEBOUNCE * 3), "clean after save");
        assert_eq!(load(&dir), Some(session.clone()));

        // Unchanged sessions are not rewritten.
        fs::remove_file(dir.join(SESSION_FILE)).expect("rm");
        saver.save(&dir, &session);
        assert!(load(&dir).is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
