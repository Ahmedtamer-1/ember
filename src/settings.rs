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

/// What Ember opens at startup (`ember://settings` → "On startup").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    /// Restore the tabs that were open when Ember last closed.
    Continue,
    /// Open a single new tab page.
    NewTab,
    /// Open the configured homepage.
    Homepage,
}

impl StartupMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::NewTab => "newtab",
            Self::Homepage => "homepage",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "newtab" => Self::NewTab,
            "homepage" => Self::Homepage,
            _ => Self::Continue,
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
    /// What to open at startup (restore the previous session by default).
    pub startup_mode: StartupMode,
    /// Let WebView2's built-in (OS-encrypted) password manager offer to save passwords.
    pub offer_save_passwords: bool,
    /// Let WebView2 autofill addresses and other form fields.
    pub autofill_enabled: bool,
    /// Clear cookies and site data (not passwords/autofill) when Ember closes.
    pub clear_site_data_on_exit: bool,
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
            startup_mode: StartupMode::Continue,
            offer_save_passwords: true,
            autofill_enabled: true,
            clear_site_data_on_exit: false,
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
             on_startup = \"{}\"\n\
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
             search_suggestions_enabled = {}\n\
             offer_save_passwords = {}\n\
             autofill_enabled = {}\n\
             clear_site_data_on_exit = {}\n",
            self.search_engine.as_str(),
            escape_toml_string(&self.homepage),
            self.theme.as_str(),
            self.tab_suspend_minutes,
            self.tab_animations_enabled,
            self.startup_mode.as_str(),
            self.adblock_enabled,
            self.easylist_enabled,
            self.easyprivacy_enabled,
            self.block_third_party_cookies,
            self.send_dnt_and_gpc,
            self.https_only_mode,
            self.search_suggestions_enabled,
            self.offer_save_passwords,
            self.autofill_enabled,
            self.clear_site_data_on_exit,
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
                        settings.tab_suspend_minutes = clamp_suspend_minutes(mins);
                    }
                }
                ("general" | "", "tab_animations_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.tab_animations_enabled = b;
                    }
                }
                ("general" | "", "on_startup") => {
                    if let Some(s) = parse_toml_string(val) {
                        settings.startup_mode = StartupMode::parse(&s);
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
                ("privacy" | "", "offer_save_passwords") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.offer_save_passwords = b;
                    }
                }
                ("privacy" | "", "autofill_enabled") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.autofill_enabled = b;
                    }
                }
                ("privacy" | "", "clear_site_data_on_exit") => {
                    if let Some(b) = parse_toml_bool(val) {
                        settings.clear_site_data_on_exit = b;
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

    /// Returns the Default profile's config directory (`%APPDATA%\Ember` on Windows).
    pub fn default_config_dir() -> io::Result<PathBuf> {
        platform::resolve_config_dir()
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
    ///
    /// A poisoned lock (a panic while another thread held it) still holds the last good
    /// settings, so the data is recovered rather than replaced with defaults.
    pub fn get(&self) -> BrowserSettings {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Mutates the settings in memory and persists the updated `settings.toml` to disk.
    ///
    /// If the lock was poisoned, the existing settings are recovered (never reset to defaults)
    /// so the user's configuration on disk cannot be wiped.
    pub fn update<F>(&self, f: F) -> BrowserSettings
    where
        F: FnOnce(&mut BrowserSettings),
    {
        let snapshot = {
            let mut guard = match self.inner.write() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    self.inner.clear_poison();
                    poisoned.into_inner()
                }
            };
            f(&mut guard);
            guard.clone()
        };
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
        if is_loopback_host(&host) {
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

/// Maximum accepted `tab_suspend_minutes` value (24 hours).
pub const MAX_TAB_SUSPEND_MINUTES: u64 = 1440;

/// Caps a user-provided tab suspend timeout at [`MAX_TAB_SUSPEND_MINUTES`] (`0` = never).
pub fn clamp_suspend_minutes(minutes: u64) -> u64 {
    minutes.min(MAX_TAB_SUSPEND_MINUTES)
}

/// Returns `true` for hosts that always refer to the local machine and are exempt from
/// HTTPS-Only Mode: `localhost`, `*.localhost`, `127.0.0.0/8`, and `[::1]`.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host == "localhost" || host.ends_with(".localhost") || host == "[::1]" {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_loopback())
}

fn strip_toml_comment(line: &str) -> &str {
    // `None` = outside a string, `Some('"')` = basic string, `Some('\'')` = literal string.
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        match quote {
            Some('"') => {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {
                if ch == '\'' {
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else if ch == '#' {
                    return &line[..idx];
                }
            }
        }
    }
    line
}

/// Reads exactly `len` hex digits from `chars` and converts them to a `char`.
fn read_unicode_escape(chars: &mut std::str::Chars<'_>, len: usize) -> Option<char> {
    let mut code: u32 = 0;
    for _ in 0..len {
        let digit = chars.next()?.to_digit(16)?;
        code = code.checked_mul(16)?.checked_add(digit)?;
    }
    char::from_u32(code)
}

fn parse_toml_string(val: &str) -> Option<String> {
    let trimmed = val.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            let Some(esc) = chars.next() else {
                // A lone trailing backslash: keep it rather than dropping data.
                out.push('\\');
                break;
            };
            match esc {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'u' => out.push(read_unicode_escape(&mut chars, 4)?),
                'U' => out.push(read_unicode_escape(&mut chars, 8)?),
                other => out.push(other),
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
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
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
    fn test_https_only_exempts_loopback_and_localhost_subdomains() {
        let dir = env::temp_dir().join(format!("ember_settings_loopback_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = SettingsStore::with_config_dir(dir.clone()).expect("init settings store");
        store.update(|s| s.https_only_mode = true);

        assert!(!store.should_intercept_http("http://[::1]:8080/"));
        assert!(!store.should_intercept_http("http://[::1]/"));
        assert!(!store.should_intercept_http("http://app.localhost:3000/"));
        assert!(!store.should_intercept_http("http://LOCALHOST/"));
        assert!(!store.should_intercept_http("http://127.0.0.1:5000/"));
        assert!(!store.should_intercept_http("http://127.1.2.3/"));
        assert!(!store.should_intercept_http("http://user:pw@localhost:8000/"));
        // Not loopback
        assert!(store.should_intercept_http("http://localhost.evil.com/"));
        assert!(store.should_intercept_http("http://127.example.com/"));
        assert!(store.should_intercept_http("http://[2001:db8::1]/"));
        // Userinfo must not fool the allowlist host.
        store.allow_http_for_url("http://neverssl.com/");
        assert!(store.should_intercept_http("http://neverssl.com@attacker.example/"));
        assert!(!store.should_intercept_http("http://user@neverssl.com/"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_update_recovers_poisoned_lock_without_wiping_settings() {
        let dir = env::temp_dir().join(format!("ember_settings_poison_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = SettingsStore::with_config_dir(dir.clone()).expect("init settings store");
        store.update(|s| {
            s.homepage = String::from("https://my.home.page/");
            s.search_engine = SearchEngine::Startpage;
        });

        let inner = Arc::clone(&store.inner);
        let _ = std::thread::spawn(move || {
            let _guard = inner.write().expect("lock");
            panic!("intentional panic to poison the settings lock");
        })
        .join();
        assert!(store.inner.is_poisoned());

        assert_eq!(store.get().homepage, "https://my.home.page/");
        let updated = store.update(|s| s.https_only_mode = true);
        assert_eq!(updated.homepage, "https://my.home.page/");
        assert_eq!(updated.search_engine, SearchEngine::Startpage);
        assert!(updated.https_only_mode);

        let reloaded = SettingsStore::with_config_dir(dir.clone()).expect("reload");
        let s = reloaded.get();
        assert_eq!(s.homepage, "https://my.home.page/");
        assert_eq!(s.search_engine, SearchEngine::Startpage);
        assert!(s.https_only_mode);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clamp_suspend_minutes() {
        assert_eq!(clamp_suspend_minutes(0), 0);
        assert_eq!(clamp_suspend_minutes(15), 15);
        assert_eq!(clamp_suspend_minutes(1440), 1440);
        assert_eq!(clamp_suspend_minutes(1441), 1440);
        assert_eq!(clamp_suspend_minutes(u64::MAX), 1440);
        assert_eq!(
            BrowserSettings::from_toml_str("[general]\ntab_suspend_minutes = 99999\n")
                .tab_suspend_minutes,
            1440
        );
    }

    #[test]
    fn test_homepage_toml_string_roundtrip() {
        let tricky = [
            "https://example.com/?q=\"quoted\"",
            "C:\\Users\\me\\start.html",
            "https://example.com/path\\",
            "https://例え.jp/ページ?q=ünïcødé😀",
            "https://example.com/#anchor 'single' \"double\"",
            "line\nbreak\ttab\u{1}ctrl\u{7f}",
        ];
        for homepage in tricky {
            let settings = BrowserSettings {
                homepage: homepage.to_owned(),
                ..BrowserSettings::default()
            };
            let parsed = BrowserSettings::from_toml_str(&settings.to_toml_string());
            assert_eq!(
                parsed.homepage, homepage,
                "roundtrip failed for {homepage:?}"
            );
            assert_eq!(
                parse_toml_string(&format!("\"{}\"", escape_toml_string(homepage))).as_deref(),
                Some(homepage)
            );
        }

        // Hand-written TOML escapes and literal strings.
        assert_eq!(
            parse_toml_string("\"caf\\u00E9 \\U0001F600\"").as_deref(),
            Some("café 😀")
        );
        assert_eq!(
            BrowserSettings::from_toml_str("homepage = 'https://x.com/#a' # comment").homepage,
            "https://x.com/#a"
        );
    }

    #[test]
    fn test_profile_password_session_fields_roundtrip() {
        let defaults = BrowserSettings::default();
        assert_eq!(defaults.startup_mode, StartupMode::Continue);
        assert!(defaults.offer_save_passwords);
        assert!(defaults.autofill_enabled);
        assert!(!defaults.clear_site_data_on_exit);

        // Settings files written before these keys existed keep the defaults.
        let old = BrowserSettings::from_toml_str("[general]\nhomepage = \"https://a.test\"\n");
        assert_eq!(old.startup_mode, StartupMode::Continue);
        assert!(old.offer_save_passwords && old.autofill_enabled);
        assert!(!old.clear_site_data_on_exit);

        for mode in [
            StartupMode::Continue,
            StartupMode::NewTab,
            StartupMode::Homepage,
        ] {
            let settings = BrowserSettings {
                startup_mode: mode,
                offer_save_passwords: false,
                autofill_enabled: false,
                clear_site_data_on_exit: true,
                ..BrowserSettings::default()
            };
            let parsed = BrowserSettings::from_toml_str(&settings.to_toml_string());
            assert_eq!(parsed, settings);
        }
        assert_eq!(StartupMode::parse("bogus"), StartupMode::Continue);
        assert_eq!(StartupMode::parse(" HomePage "), StartupMode::Homepage);
        assert_eq!(StartupMode::parse("newtab"), StartupMode::NewTab);
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
