use std::{
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::storage::DEFAULT_TAB_SUSPEND_MINUTES;

/// Supported default search engines in `ember://settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchEngine {
    DuckDuckGo,
    Google,
    Brave,
    Bing,
    Startpage,
}

impl SearchEngine {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuckDuckGo => "duckduckgo",
            Self::Google => "google",
            Self::Brave => "brave",
            Self::Bing => "bing",
            Self::Startpage => "startpage",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "google" => Self::Google,
            "brave" => Self::Brave,
            "bing" => Self::Bing,
            "startpage" => Self::Startpage,
            _ => Self::DuckDuckGo,
        }
    }

    pub fn build_search_url(self, encoded_query: &str) -> String {
        match self {
            Self::DuckDuckGo => format!("https://duckduckgo.com/?q={encoded_query}"),
            Self::Google => format!("https://www.google.com/search?q={encoded_query}"),
            Self::Brave => format!("https://search.brave.com/search?q={encoded_query}"),
            Self::Bing => format!("https://www.bing.com/search?q={encoded_query}"),
            Self::Startpage => format!("https://www.startpage.com/sp/search?query={encoded_query}"),
        }
    }
}

/// User theme preference in `ember://settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    pub const fn resolve_is_dark(self, os_is_dark: bool) -> bool {
        match self {
            Self::System => os_is_dark,
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

/// Complete browser settings persisted in `settings.toml` inside the OS config directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSettings {
    pub search_engine: SearchEngine,
    pub homepage: String,
    pub theme: ThemePreference,
    pub tab_suspend_minutes: u64,
    pub adblock_enabled: bool,
    pub easylist_enabled: bool,
    pub easyprivacy_enabled: bool,
    pub block_third_party_cookies: bool,
    pub send_dnt_and_gpc: bool,
    pub https_only_mode: bool,
    pub tab_animations_enabled: bool,
    pub search_suggestions_enabled: bool,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        let suspend_minutes = env::var("EMBER_SUSPEND_MINUTES")
            .ok()
            .and_then(|val| val.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_TAB_SUSPEND_MINUTES);

        Self {
            search_engine: SearchEngine::DuckDuckGo,
            homepage: String::from("ember://newtab"),
            theme: ThemePreference::System,
            tab_suspend_minutes: suspend_minutes,
            adblock_enabled: true,
            easylist_enabled: true,
            easyprivacy_enabled: true,
            block_third_party_cookies: true,
            send_dnt_and_gpc: true,
            https_only_mode: false,
            tab_animations_enabled: true,
            search_suggestions_enabled: false,
        }
    }
}

impl BrowserSettings {
    /// Returns the inactivity `Duration` before background tabs are suspended.
    /// When `tab_suspend_minutes == 0` ("Never"), returns a 365-day duration.
    pub fn tab_suspend_timeout(&self) -> Duration {
        if self.tab_suspend_minutes == 0 {
            Duration::from_secs(365 * 24 * 3600)
        } else {
            Duration::from_secs(self.tab_suspend_minutes.saturating_mul(60))
        }
    }

    /// Serializes the settings into a human-readable TOML document.
    pub fn to_toml_string(&self) -> String {
        format!(
            "# Ember Browser Configuration (settings.toml)\n\
             \n\
             [general]\n\
             search_engine = \"{}\"\n\
             homepage = \"{}\"\n\
             theme = \"{}\"\n\
             tab_suspend_minutes = {}\n\
             tab_animations_enabled = {}\n\
             \n\
             [adblock]\n\
             enabled = {}\n\
             easylist_enabled = {}\n\
             easyprivacy_enabled = {}\n\
             \n\
             [privacy]\n\
             block_third_party_cookies = {}\n\
             send_dnt_and_gpc = {}\n\
             https_only_mode = {}\n\
             search_suggestions_enabled = {}\n",
            self.search_engine.as_str(),
            escape_toml_string(&self.homepage),
            self.theme.as_str(),
            self.tab_suspend_minutes,
            self.tab_animations_enabled,
            self.adblock_enabled,
            self.easylist_enabled,
            self.easyprivacy_enabled,
            self.block_third_party_cookies,
            self.send_dnt_and_gpc,
            self.https_only_mode,
            self.search_suggestions_enabled,
        )
    }

    /// Parses `BrowserSettings` from a TOML string, falling back to defaults for any missing keys.
    pub fn from_toml_str(raw: &str) -> Self {
        let mut settings = Self::default();
        let mut current_section = String::new();

        for line in raw.lines() {
            let trimmed = strip_toml_comment(line).trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                current_section = trimmed[1..trimmed.len() - 1].trim().to_ascii_lowercase();
                continue;
            }
            let Some((raw_key, raw_val)) = trimmed.split_once('=') else {
                continue;
            };
            let key = raw_key.trim().to_ascii_lowercase();
            let val = raw_val.trim();

            match (current_section.as_str(), key.as_str()) {
                ("general" | "", "search_engine") => {
                    if let Some(s) = parse_toml_string(val) {
                        settings.search_engine = SearchEngine::parse(&s);
                    }
                }
                ("general" | "", "homepage") => {
                    if let Some(s) = parse_toml_string(val) {
                        if !s.trim().is_empty() {
                            settings.homepage = s.trim().to_owned();
                        }
                    }
                }
                ("general" | "", "theme") => {
                    if let Some(s) = parse_toml_string(val) {
                        settings.theme = ThemePreference::parse(&s);
                    }
                }
                ("general" | "", "tab_suspend_minutes") => {
                    if let Ok(mins) = val.parse::<u64>() {
                        settings.tab_suspend_minutes = mins.min(1440);
                    }
                }
                ("general" | "", "tab_animations_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.tab_animations_enabled = b;
                    }
                }
                ("adblock" | "", "enabled" | "adblock_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.adblock_enabled = b;
                    }
                }
                ("adblock" | "", "easylist_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.easylist_enabled = b;
                    }
                }
                ("adblock" | "", "easyprivacy_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.easyprivacy_enabled = b;
                    }
                }
                ("privacy" | "", "block_third_party_cookies") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.block_third_party_cookies = b;
                    }
                }
                ("privacy" | "", "send_dnt_and_gpc") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.send_dnt_and_gpc = b;
                    }
                }
                ("privacy" | "", "https_only_mode") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.https_only_mode = b;
                    }
                }
                ("privacy" | "", "search_suggestions_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.search_suggestions_enabled = b;
                    }
                }
                _ => {}
            }
        }

        settings
    }
}

/// Thread-safe handle to the browser's persistent TOML settings and session HTTP allowlist.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    config_path: PathBuf,
    inner: Arc<RwLock<BrowserSettings>>,
    http_allowed_hosts: Arc<RwLock<HashSet<String>>>,
}

impl SettingsStore {
    /// Loads `%APPDATA%\Ember\settings.toml` (or creates it with default settings if missing).
    pub fn init() -> io::Result<Self> {
        let config_dir = platform::resolve_config_dir()?;
        Self::with_config_dir(config_dir)
    }

    /// Initializes the settings store inside a specific config directory.
    pub fn with_config_dir(config_dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("settings.toml");

        let settings = if let Ok(raw) = fs::read_to_string(&config_path) {
            BrowserSettings::from_toml_str(&raw)
        } else {
            let default_settings = BrowserSettings::default();
            let _ = fs::write(&config_path, default_settings.to_toml_string());
            default_settings
        };

        Ok(Self {
            config_path,
            inner: Arc::new(RwLock::new(settings)),
            http_allowed_hosts: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    /// Returns the path to `settings.toml`.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Returns a snapshot of the current `BrowserSettings`.
    pub fn get(&self) -> BrowserSettings {
        self.inner.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Mutates the settings in memory and persists the updated `settings.toml` to disk.
    pub fn update<F>(&self, f: F) -> BrowserSettings
    where
        F: FnOnce(&mut BrowserSettings),
    {
        let mut snapshot = BrowserSettings::default();
        if let Ok(mut guard) = self.inner.write() {
            f(&mut guard);
            snapshot = guard.clone();
        }
        let _ = fs::write(&self.config_path, snapshot.to_toml_string());
        snapshot
    }

    /// Returns `true` if `url` is an insecure `http://` URL that should be intercepted by HTTPS-Only Mode.
    pub fn should_intercept_http(&self, url: &str) -> bool {
        let settings = self.get();
        if !settings.https_only_mode {
            return false;
        }
        let trimmed = url.trim();
        if !trimmed.to_ascii_lowercase().starts_with("http://") {
            return false;
        }
        let Some(host) = crate::zoom::extract_host(trimmed) else {
            return false;
        };
        if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
            return false;
        }
        let allowed = self
            .http_allowed_hosts
            .read()
            .map(|set| set.contains(&host))
            .unwrap_or(false);
        !allowed
    }

    /// Temporarily allows insecure `http://` navigation to `url`'s host for the current session.
    pub fn allow_http_for_url(&self, url: &str) {
        if let Some(host) = crate::zoom::extract_host(url) {
            if let Ok(mut set) = self.http_allowed_hosts.write() {
                set.insert(host);
            }
        }
    }
}

fn strip_toml_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == '#' {
            return &line[..idx];
        }
    }
    line
}

fn parse_toml_string(val: &str) -> Option<String> {
    let trimmed = val.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                match ch {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    _ => out.push(ch),
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                out.push(ch);
            }
        }
        Some(out)
    } else if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        Some(trimmed[1..trimmed.len() - 1].to_owned())
    } else {
        None
    }
}

fn parse_toml_bool(val: &str) -> Option<bool> {
    match val.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn escape_toml_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_config_dir() -> io::Result<PathBuf> {
        if let Some(appdata) = env::var_os("APPDATA") {
            return Ok(PathBuf::from(appdata).join("Ember"));
        }
        if let Some(local) = env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(local).join("Ember"));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Neither %APPDATA% nor %LOCALAPPDATA% is set",
        ))
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_config_dir() -> io::Result<PathBuf> {
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
}

#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
mod platform {
    use std::{env, io, path::PathBuf};

    pub fn resolve_config_dir() -> io::Result<PathBuf> {
        if let Some(xdg_config) = env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config).join("Ember"));
        }
        let home = env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Neither $XDG_CONFIG_HOME nor $HOME is set",
            )
        })?;
        Ok(PathBuf::from(home).join(".config").join("Ember"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_toml_roundtrip_and_persistence() {
        let dir = env::temp_dir().join(format!("ember_settings_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let store = SettingsStore::with_config_dir(dir.clone()).expect("init settings store");
        assert!(store.config_path().exists());

        store.update(|s| {
            s.search_engine = SearchEngine::Brave;
            s.homepage = String::from("https://rust-lang.org");
            s.theme = ThemePreference::Dark;
            s.tab_suspend_minutes = 15;
            s.easylist_enabled = true;
            s.easyprivacy_enabled = false;
            s.block_third_party_cookies = true;
            s.send_dnt_and_gpc = true;
            s.https_only_mode = true;
            s.tab_animations_enabled = false;
        });

        let reloaded = SettingsStore::with_config_dir(dir.clone()).expect("reload settings store");
        let s = reloaded.get();
        assert_eq!(s.search_engine, SearchEngine::Brave);
        assert_eq!(s.homepage, "https://rust-lang.org");
        assert_eq!(s.theme, ThemePreference::Dark);
        assert_eq!(s.tab_suspend_minutes, 15);
        assert!(!s.tab_animations_enabled);
        assert!(s.easylist_enabled);
        assert!(!s.easyprivacy_enabled);
        assert!(s.block_third_party_cookies);
        assert!(s.send_dnt_and_gpc);
        assert!(s.https_only_mode);

        // HTTPS-Only Mode interception & per-host bypass
        assert!(reloaded.should_intercept_http("http://neverssl.com/"));
        assert!(!reloaded.should_intercept_http("https://neverssl.com/"));
        assert!(!reloaded.should_intercept_http("http://localhost:8080/"));

        reloaded.allow_http_for_url("http://neverssl.com/");
        assert!(!reloaded.should_intercept_http("http://neverssl.com/page"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tab_animations_enabled_default_and_missing_toml_key() {
        assert!(BrowserSettings::default().tab_animations_enabled);

        // Empty TOML and TOML missing `tab_animations_enabled` must preserve default true.
        assert!(BrowserSettings::from_toml_str("").tab_animations_enabled);
        let without_key = BrowserSettings::from_toml_str(
            "[general]\nhomepage = \"https://example.com\"\n[privacy]\nhttps_only_mode = true\n",
        );
        assert!(without_key.tab_animations_enabled);
        assert!(without_key.https_only_mode);

        // Explicit boolean values and invalid fallback.
        assert!(
            !BrowserSettings::from_toml_str("[general]\ntab_animations_enabled = false\n")
                .tab_animations_enabled
        );
        assert!(
            BrowserSettings::from_toml_str("[general]\ntab_animations_enabled = true\n")
                .tab_animations_enabled
        );
        assert!(
            BrowserSettings::from_toml_str("[general]\ntab_animations_enabled = \"invalid\"\n")
                .tab_animations_enabled
        );
    }
}
