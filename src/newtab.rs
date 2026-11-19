//! Customizable new tab page (`ember://newtab`) configuration.
//!
//! The configuration is a flat set of whitelisted `key=value` settings plus an ordered list of
//! user shortcuts. Every value is validated before it is stored: numbers are clamped, choices
//! are checked against a fixed list, colors use the theme color validator, free text has
//! control characters stripped and is length-limited, and shortcut URLs must be `http(s)`.
//! Values are still HTML/JS-escaped when the page is rendered.
//!
//! The page's "Customize" panel sends the whole configuration over privileged internal IPC
//! (`newtab_save:<payload>`), where the payload uses the same line format as `newtab.txt`
//! (stored next to `settings.toml`).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Maximum number of user shortcuts.
pub const MAX_SHORTCUTS: usize = 48;
/// Maximum shortcut title length (characters).
pub const MAX_TITLE_LEN: usize = 80;
/// Maximum shortcut URL length (bytes).
pub const MAX_URL_LEN: usize = 2048;
/// Maximum quick-notes length (characters).
pub const MAX_NOTES_LEN: usize = 4000;
/// Maximum greeting name length (characters).
pub const MAX_NAME_LEN: usize = 40;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Flag,
    Choice(&'static [&'static str]),
    /// Integer clamped to `[min, max]`.
    Number(i64, i64),
    /// Single-line text up to N characters (empty allowed).
    Text(usize),
    /// Multi-line text up to N characters (empty allowed).
    MultiText(usize),
    /// A CSS color, or empty for the theme default.
    Color,
}

/// `(key, kind, default)`.
const KEYS: &[(&str, Kind, &str)] = &[
    ("layout", Kind::Choice(&["centered", "top"]), "centered"),
    ("show_logo", Kind::Flag, "true"),
    ("show_clock", Kind::Flag, "true"),
    ("clock_24h", Kind::Flag, "true"),
    ("clock_seconds", Kind::Flag, "false"),
    ("show_date", Kind::Flag, "true"),
    ("show_greeting", Kind::Flag, "false"),
    ("greeting_name", Kind::Text(MAX_NAME_LEN), ""),
    ("show_search", Kind::Flag, "true"),
    ("show_shortcuts", Kind::Flag, "true"),
    ("show_most_visited", Kind::Flag, "false"),
    ("show_notes", Kind::Flag, "false"),
    (
        "search_style",
        Kind::Choice(&["rounded", "pill", "square"]),
        "pill",
    ),
    (
        "search_size",
        Kind::Choice(&["small", "medium", "large"]),
        "medium",
    ),
    ("search_opacity", Kind::Number(20, 100), "100"),
    ("tile_size", Kind::Number(40, 96), "56"),
    ("tile_columns", Kind::Number(2, 10), "6"),
    (
        "tile_style",
        Kind::Choice(&["icon", "icon_label"]),
        "icon_label",
    ),
    ("tile_radius", Kind::Number(0, 48), "16"),
    ("tile_translucent", Kind::Flag, "false"),
    (
        "source",
        Kind::Choice(&["mine", "most_visited", "both"]),
        "both",
    ),
    ("text_color", Kind::Color, ""),
    ("font_scale", Kind::Number(80, 140), "100"),
    ("notes", Kind::MultiText(MAX_NOTES_LEN), ""),
];

fn kind_of(key: &str) -> Option<Kind> {
    KEYS.iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, kind, _)| *kind)
}

fn default_of(key: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _, _)| *k == key).map(|(_, _, d)| *d)
}

/// Returns all known setting keys.
pub fn keys() -> impl Iterator<Item = &'static str> {
    KEYS.iter().map(|(k, _, _)| *k)
}

/// Replaces control characters (except `\n` / `\t` when `multiline`) with spaces and truncates to
/// `max` characters.
fn clean_text(value: &str, max: usize, multiline: bool) -> String {
    value
        .chars()
        .filter(|c| *c != '\r')
        .map(|c| {
            if (multiline && matches!(c, '\n' | '\t')) || !c.is_control() {
                c
            } else {
                ' '
            }
        })
        .take(max)
        .collect()
}

/// Validates and normalizes `value` for `key`. Returns `None` for unknown keys or invalid values.
pub fn normalize_value(key: &str, value: &str) -> Option<String> {
    match kind_of(key)? {
        Kind::Flag => match value.trim() {
            "true" | "1" | "on" => Some(String::from("true")),
            "false" | "0" | "off" => Some(String::from("false")),
            _ => None,
        },
        Kind::Choice(options) => options
            .iter()
            .find(|o| o.eq_ignore_ascii_case(value.trim()))
            .map(|o| (*o).to_owned()),
        Kind::Number(min, max) => {
            let n = value.trim().parse::<f64>().ok().filter(|n| n.is_finite())?;
            // Clamp in f64 first so the cast below cannot saturate unexpectedly.
            let n = n.round().clamp(min as f64, max as f64) as i64;
            Some(n.to_string())
        }
        Kind::Text(max) => Some(clean_text(value.trim(), max, false).trim().to_owned()),
        Kind::MultiText(max) => Some(clean_text(value, max, true)),
        Kind::Color => {
            let v = value.trim();
            if v.is_empty() {
                Some(String::new())
            } else {
                crate::theme::is_valid_color(v).then(|| v.to_owned())
            }
        }
    }
}

/// Validates a shortcut URL: `http://` or `https://` with a host, no whitespace, control
/// characters, quotes or angle brackets, at most [`MAX_URL_LEN`] bytes.
pub fn is_valid_shortcut_url(url: &str) -> bool {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return false;
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '`' | '\\'))
    {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    let rest = match lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    {
        Some(rest) => rest,
        None => return false,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop optional userinfo.
    let host = host.rsplit('@').next().unwrap_or(host);
    !host.is_empty() && !host.starts_with(':')
}

/// A user-managed shortcut tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    pub title: String,
    pub url: String,
}

impl Shortcut {
    /// Builds a validated shortcut; `None` if the URL is not acceptable. An empty title falls
    /// back to the URL's host.
    pub fn new(title: &str, url: &str) -> Option<Self> {
        let url = url.trim();
        if !is_valid_shortcut_url(url) {
            return None;
        }
        let mut title = clean_text(title.trim(), MAX_TITLE_LEN, false)
            .trim()
            .to_owned();
        if title.is_empty() {
            let after = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
            let host = after.split(['/', '?', '#']).next().unwrap_or(after);
            title = clean_text(
                host.strip_prefix("www.").unwrap_or(host),
                MAX_TITLE_LEN,
                false,
            );
        }
        Some(Self {
            title,
            url: url.to_owned(),
        })
    }
}

/// Escapes `\` and newlines so a value fits on one line.
fn encode_line_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn decode_line_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A validated new tab page configuration.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewtabConfig {
    values: BTreeMap<String, String>,
    shortcuts: Vec<Shortcut>,
}

impl NewtabConfig {
    /// Parses `key=value` lines. `shortcut=<url>\t<title>` lines (in order) form the shortcut
    /// list. Unknown keys, invalid values and extra shortcuts are dropped.
    pub fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            if k == "shortcut" {
                let v = v.trim();
                let (url, title) = v.split_once('\t').unwrap_or((v, ""));
                cfg.push_shortcut(&decode_line_value(title), url);
            } else {
                cfg.set(k, &decode_line_value(v));
            }
        }
        cfg
    }

    /// Serializes to the line format accepted by [`NewtabConfig::parse`].
    pub fn serialize(&self) -> String {
        let mut out =
            String::from("# Ember new tab page (edit with Customize on ember://newtab)\n");
        for (k, v) in &self.values {
            out.push_str(k);
            out.push('=');
            out.push_str(&encode_line_value(v));
            out.push('\n');
        }
        for s in &self.shortcuts {
            out.push_str("shortcut=");
            out.push_str(&s.url);
            out.push('\t');
            out.push_str(&encode_line_value(&s.title));
            out.push('\n');
        }
        out
    }

    /// Sets `key` if the value is valid. Returns `true` if accepted.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        match normalize_value(key, value) {
            Some(v) => {
                if default_of(key) == Some(v.as_str()) {
                    self.values.remove(key);
                } else {
                    self.values.insert(key.to_owned(), v);
                }
                true
            }
            None => false,
        }
    }

    /// Returns the value for `key` (stored or default). Unknown keys yield `""`.
    pub fn get(&self, key: &str) -> &str {
        self.values
            .get(key)
            .map(String::as_str)
            .or_else(|| default_of(key))
            .unwrap_or("")
    }

    /// Returns a boolean setting.
    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == "true"
    }

    /// Returns a numeric setting (0 if unknown).
    pub fn number(&self, key: &str) -> i64 {
        self.get(key).parse().unwrap_or(0)
    }

    /// Appends a validated shortcut. Returns `false` if invalid or the list is full.
    pub fn push_shortcut(&mut self, title: &str, url: &str) -> bool {
        if self.shortcuts.len() >= MAX_SHORTCUTS {
            return false;
        }
        match Shortcut::new(title, url) {
            Some(s) => {
                self.shortcuts.push(s);
                true
            }
            None => false,
        }
    }

    /// The user's shortcuts, in display order.
    pub fn shortcuts(&self) -> &[Shortcut] {
        &self.shortcuts
    }
}

/// Parses a `newtab_save:` IPC payload.
pub fn parse_payload(payload: &str) -> NewtabConfig {
    NewtabConfig::parse(payload)
}

struct NewtabState {
    path: Option<PathBuf>,
    config: NewtabConfig,
}

fn state() -> &'static Mutex<NewtabState> {
    static STATE: OnceLock<Mutex<NewtabState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(NewtabState {
            path: None,
            config: NewtabConfig::default(),
        })
    })
}

/// Loads `newtab.txt` from `config_dir` into the process-wide configuration.
pub fn init(config_dir: &Path) {
    let path = config_dir.join("newtab.txt");
    let config = fs::read_to_string(&path)
        .map(|t| NewtabConfig::parse(&t))
        .unwrap_or_default();
    if let Ok(mut s) = state().lock() {
        s.path = Some(path);
        s.config = config;
    }
}

/// Returns a copy of the current configuration.
pub fn current() -> NewtabConfig {
    state().lock().map(|s| s.config.clone()).unwrap_or_default()
}

/// Replaces the current configuration and writes it to disk.
pub fn save(config: NewtabConfig) -> io::Result<()> {
    let path = {
        let mut s = state()
            .lock()
            .map_err(|_| io::Error::other("newtab state poisoned"))?;
        s.config = config.clone();
        s.path.clone()
    };
    if let Some(path) = path {
        let tmp = path.with_extension("txt.tmp");
        fs::write(&tmp, config.serialize())?;
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_validated_and_clamped() {
        assert_eq!(normalize_value("tile_columns", "99").as_deref(), Some("10"));
        assert_eq!(normalize_value("tile_columns", "-3").as_deref(), Some("2"));
        assert_eq!(normalize_value("tile_columns", "NaN"), None);
        assert_eq!(normalize_value("tile_columns", "inf"), None);
        assert_eq!(normalize_value("font_scale", "1e9").as_deref(), Some("140"));
        assert_eq!(normalize_value("layout", "TOP").as_deref(), Some("top"));
        assert_eq!(normalize_value("layout", "sideways"), None);
        assert_eq!(normalize_value("show_clock", "maybe"), None);
        assert_eq!(normalize_value("unknown_key", "1"), None);
        assert_eq!(
            normalize_value("text_color", "#fff").as_deref(),
            Some("#fff")
        );
        assert_eq!(normalize_value("text_color", "").as_deref(), Some(""));
        assert_eq!(normalize_value("text_color", "red;}body{x:y"), None);
        let long: String = "x".repeat(500);
        assert_eq!(
            normalize_value("greeting_name", &long).map(|v| v.chars().count()),
            Some(MAX_NAME_LEN)
        );
        assert_eq!(
            normalize_value("greeting_name", "Ann\u{0}\tB").as_deref(),
            Some("Ann  B")
        );
        assert_eq!(normalize_value("notes", "a\r\nb").as_deref(), Some("a\nb"));
    }

    #[test]
    fn shortcut_urls_are_restricted_to_http() {
        for ok in [
            "https://example.com",
            "http://localhost:8080/x?y=1#z",
            "HTTPS://Example.com/path",
        ] {
            assert!(is_valid_shortcut_url(ok), "{ok}");
        }
        for bad in [
            "javascript:alert(1)",
            "JavaScript://example.com/%0aalert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///C:/Windows/win.ini",
            "ember://settings",
            "about:blank",
            "https://",
            "https:///path",
            "https://exa mple.com",
            "https://example.com/\"onmouseover=x",
            "https://example.com/<script>",
            "",
        ] {
            assert!(!is_valid_shortcut_url(bad), "{bad}");
        }
        assert!(Shortcut::new("x", "javascript:alert(1)").is_none());
        assert_eq!(
            Shortcut::new("", "https://www.example.com/a").map(|s| s.title),
            Some(String::from("example.com"))
        );
    }

    #[test]
    fn shortcut_count_is_capped() {
        let mut cfg = NewtabConfig::default();
        for i in 0..60 {
            cfg.push_shortcut("t", &format!("https://s{i}.test"));
        }
        assert_eq!(cfg.shortcuts().len(), MAX_SHORTCUTS);
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!("shortcut=https://s{i}.test\tt\n"));
        }
        assert_eq!(NewtabConfig::parse(&text).shortcuts().len(), MAX_SHORTCUTS);
    }

    #[test]
    fn roundtrip_preserves_values_shortcuts_and_notes() {
        let mut cfg = NewtabConfig::default();
        assert!(cfg.set("layout", "top"));
        assert!(cfg.set("tile_columns", "4"));
        assert!(cfg.set("text_color", "rgba(255,255,255,0.9)"));
        assert!(cfg.set("greeting_name", "Zoë"));
        assert!(cfg.set("notes", "line 1\nline \\n 2\n\ttabbed"));
        assert!(cfg.push_shortcut("A <b> \"title\"", "https://a.test/?q=1&r=2"));
        assert!(cfg.push_shortcut("", "http://b.test"));
        assert!(!cfg.push_shortcut("evil", "javascript:alert(1)"));
        let back = NewtabConfig::parse(&cfg.serialize());
        assert_eq!(back, cfg);
        assert_eq!(back.get("notes"), "line 1\nline \\n 2\n\ttabbed");
        assert_eq!(back.shortcuts()[0].title, "A <b> \"title\"");
        assert_eq!(back.shortcuts()[1].title, "b.test");
        // Defaults are not stored but are returned by get().
        assert!(cfg.set("layout", "centered"));
        assert!(!cfg.serialize().contains("layout="));
        assert_eq!(cfg.get("layout"), "centered");
        assert!(cfg.flag("show_search"));
        assert_eq!(cfg.number("tile_columns"), 4);
    }

    #[test]
    fn parse_drops_unknown_and_invalid_lines() {
        let cfg = NewtabConfig::parse(
            "evil=<script>\nlayout=diagonal\nshortcut=file:///etc/passwd\tx\nshortcut=data:text/html,x\tx\nshow_clock=false\n",
        );
        assert_eq!(cfg.get("layout"), "centered");
        assert!(!cfg.flag("show_clock"));
        assert!(cfg.shortcuts().is_empty());
        assert!(!cfg.serialize().contains("evil"));
    }
}
