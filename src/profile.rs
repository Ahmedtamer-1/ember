//! Browser profiles, private windows and the per-profile single-instance lock.
//!
//! * Every profile is fully isolated: its own `settings.toml`, `theme.txt`, `session.txt`
//!   (config dir) and `ember.db`, adblock state and WebView2 user data folder (data dir).
//! * The `Default` profile uses the historical paths (`%APPDATA%\Ember` and
//!   `%LOCALAPPDATA%\Ember`), so existing users keep their data. Other profiles live in a
//!   `Profiles\<name>` subdirectory of both roots.
//! * A profile is selected with `--profile=<name>` and always runs in its own process/window.
//!   An exclusive lock file in the profile's data dir prevents two processes from sharing (and
//!   corrupting) one profile.
//! * `--private` runs a private window on a throw-away directory in `%TEMP%` that is deleted
//!   after the window closes (by a detached `--cleanup-temp-dir=<dir>` helper process, with a
//!   startup sweep of stale directories as a backstop).

use std::{
    env,
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime},
};

/// Name of the profile that uses the pre-profiles data locations.
pub const DEFAULT_PROFILE: &str = "Default";
/// Maximum length of a profile name, in characters.
pub const MAX_PROFILE_NAME_LEN: usize = 32;
/// Subdirectory (of both the config root and the data root) holding non-default profiles.
const PROFILES_SUBDIR: &str = "Profiles";
/// Lock file held (exclusively) by the process that owns a profile / temporary directory.
const LOCK_FILE: &str = "ember.lock";
/// Marker file: site data may exist that "clear cookies and site data on close" must remove.
const SITE_DATA_MARKER: &str = "clear_site_data_pending";
/// Temporary directory prefix for private windows.
pub const PRIVATE_DIR_PREFIX: &str = "ember_private_";
/// Temporary directory prefix for `--selftest` runs.
pub const SELFTEST_DIR_PREFIX: &str = "ember_selftest_";
/// Temporary directories younger than this are never swept (their owner may still be
/// starting up and not hold the lock yet).
const SWEEP_MIN_AGE: Duration = Duration::from_secs(120);

/// Why a profile name was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileNameError {
    Empty,
    TooLong,
    InvalidCharacter,
    Reserved,
}

impl ProfileNameError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a profile name.",
            Self::TooLong => "Profile names can be at most 32 characters long.",
            Self::InvalidCharacter => {
                "Profile names may only contain letters, digits, spaces, dashes and underscores."
            }
            Self::Reserved => "That name is reserved. Choose another profile name.",
        }
    }
}

/// Validates a profile name and returns it trimmed.
///
/// Allowed: 1 to [`MAX_PROFILE_NAME_LEN`] ASCII letters, digits, spaces, `-` and `_`, not
/// starting or ending with a space (after trimming the input) and not a Windows device name
/// (`CON`, `NUL`, `COM1`, ...). `Default` (any case) is accepted and refers to the default
/// profile; see [`is_default_profile`].
pub fn validate_profile_name(raw: &str) -> Result<String, ProfileNameError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ProfileNameError::Empty);
    }
    if name.chars().count() > MAX_PROFILE_NAME_LEN {
        return Err(ProfileNameError::TooLong);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_')
    {
        return Err(ProfileNameError::InvalidCharacter);
    }
    const DEVICE_NAMES: &[&str] = &["CON", "PRN", "AUX", "NUL"];
    let upper = name.to_ascii_uppercase();
    let is_numbered_device = (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && upper.as_bytes()[3].is_ascii_digit();
    if DEVICE_NAMES.contains(&upper.as_str()) || is_numbered_device {
        return Err(ProfileNameError::Reserved);
    }
    Ok(name.to_owned())
}

/// Returns `true` if `name` refers to the default profile (case-insensitive).
pub fn is_default_profile(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(DEFAULT_PROFILE)
}

/// Options parsed from the process command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Selected profile (`--profile=<name>`); `None` means the default profile.
    pub profile: Option<String>,
    /// `--private`: run a private window.
    pub private: bool,
    /// `--cleanup-temp-dir=<dir>`: run as the helper that deletes a private window's data.
    pub cleanup_dir: Option<PathBuf>,
}

/// Parses Ember's profile-related command-line flags (other arguments are ignored).
pub fn parse_launch_args<I, S>(args: I) -> Result<LaunchArgs, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = LaunchArgs::default();
    for arg in args {
        let arg = arg.as_ref();
        if let Some(raw) = arg.strip_prefix("--profile=") {
            let name = validate_profile_name(raw)
                .map_err(|err| format!("Invalid profile name {raw:?}: {}", err.message()))?;
            out.profile = (!is_default_profile(&name)).then_some(name);
        } else if arg == "--private" {
            out.private = true;
        } else if let Some(dir) = arg.strip_prefix("--cleanup-temp-dir=") {
            out.cleanup_dir = Some(PathBuf::from(dir));
        }
    }
    Ok(out)
}

/// Base directories under which all persistent profiles live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRoots {
    /// Default profile config dir (`%APPDATA%\Ember`).
    pub config_root: PathBuf,
    /// Default profile data dir (`%LOCALAPPDATA%\Ember`).
    pub data_root: PathBuf,
}

impl ProfileRoots {
    /// Resolves the OS-specific roots used by the default profile.
    pub fn resolve() -> io::Result<Self> {
        Ok(Self {
            config_root: crate::settings::SettingsStore::default_config_dir()?,
            data_root: crate::storage::Storage::default_data_dir()?,
        })
    }

    /// Returns `(config_dir, data_dir)` for profile `name` (which must be validated).
    pub fn dirs_for(&self, name: &str) -> (PathBuf, PathBuf) {
        if is_default_profile(name) {
            (self.config_root.clone(), self.data_root.clone())
        } else {
            (
                self.config_root.join(PROFILES_SUBDIR).join(name),
                self.data_root.join(PROFILES_SUBDIR).join(name),
            )
        }
    }

    /// Lists all profiles: `Default` first, then the others sorted case-insensitively.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.config_root.join(PROFILES_SUBDIR))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| validate_profile_name(n).is_ok_and(|v| v == *n))
                    .filter(|n| !is_default_profile(n))
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names.insert(0, String::from(DEFAULT_PROFILE));
        names
    }

    /// Returns the stored spelling of an existing profile matching `name` case-insensitively.
    pub fn find_existing(&self, name: &str) -> Option<String> {
        self.list()
            .into_iter()
            .find(|n| n.eq_ignore_ascii_case(name.trim()))
    }

    /// Resolves a validated name to the profile's canonical spelling (existing profile) or the
    /// name itself (a profile that will be created on first launch).
    pub fn canonical_name(&self, name: &str) -> String {
        self.find_existing(name)
            .unwrap_or_else(|| name.trim().to_owned())
    }

    /// Creates a new profile. Returns its name.
    pub fn create(&self, raw: &str) -> Result<String, String> {
        let name = validate_profile_name(raw).map_err(|e| e.message().to_owned())?;
        if is_default_profile(&name) {
            return Err(String::from("The Default profile already exists."));
        }
        if self.find_existing(&name).is_some() {
            return Err(format!("A profile named \"{name}\" already exists."));
        }
        let (config_dir, data_dir) = self.dirs_for(&name);
        fs::create_dir_all(&config_dir)
            .and_then(|()| fs::create_dir_all(&data_dir))
            .map_err(|e| format!("Could not create profile \"{name}\": {e}"))?;
        Ok(name)
    }

    /// Permanently deletes profile `raw` and all of its data. The default profile, the profile
    /// of the calling process (`current`) and profiles open in another window are refused.
    pub fn delete(&self, raw: &str, current: &str) -> Result<String, String> {
        let name = validate_profile_name(raw).map_err(|e| e.message().to_owned())?;
        if is_default_profile(&name) {
            return Err(String::from("The Default profile cannot be deleted."));
        }
        let Some(name) = self.find_existing(&name) else {
            return Err(format!("There is no profile named \"{name}\"."));
        };
        if name.eq_ignore_ascii_case(current) {
            return Err(String::from(
                "The profile used by this window cannot be deleted.",
            ));
        }
        let (config_dir, data_dir) = self.dirs_for(&name);
        if data_dir.exists() {
            match try_lock_dir(&data_dir) {
                Ok(Some(lock)) => drop(lock),
                Ok(None) => {
                    return Err(format!(
                        "\"{name}\" is open in another window. Close it first."
                    ))
                }
                Err(e) => return Err(format!("Could not delete \"{name}\": {e}")),
            }
            fs::remove_dir_all(&data_dir)
                .map_err(|e| format!("Could not delete \"{name}\": {e}"))?;
        }
        if config_dir.exists() {
            fs::remove_dir_all(&config_dir)
                .map_err(|e| format!("Could not delete \"{name}\": {e}"))?;
        }
        Ok(name)
    }
}

/// What kind of window this process runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// A persistent profile (Default or named).
    Normal,
    /// A private window on a temporary directory.
    Private,
    /// `--selftest` run on a temporary directory.
    SelfTest,
}

/// The profile this process runs.
#[derive(Debug, Clone)]
pub struct ProfileContext {
    /// Profile name (`Default`, `Work`, ...). Private windows keep the name of the profile
    /// whose settings they copied.
    pub name: String,
    pub kind: ProfileKind,
    /// Directory holding `settings.toml`, `theme.txt` and `session.txt`.
    pub config_dir: PathBuf,
    /// Directory holding `ember.db`, filters and the WebView2 user data folder.
    pub data_dir: PathBuf,
    /// Roots of the persistent profiles (`None` for self-tests).
    pub roots: Option<ProfileRoots>,
}

impl ProfileContext {
    pub fn is_private(&self) -> bool {
        self.kind == ProfileKind::Private
    }

    /// Only normal profiles restore and save sessions (never private windows or self-tests).
    pub fn persists_session(&self) -> bool {
        self.kind == ProfileKind::Normal
    }

    /// Window title used when no page title is available: `Ember`, `Ember — Work` or
    /// `Ember — Private`.
    pub fn window_title_base(&self) -> String {
        match self.badge_text() {
            Some(badge) => format!("Ember \u{2014} {badge}"),
            None => String::from("Ember"),
        }
    }

    /// Text of the badge shown in the tab strip (`Private` or a non-default profile name).
    pub fn badge_text(&self) -> Option<&str> {
        match self.kind {
            ProfileKind::Private => Some("Private"),
            ProfileKind::Normal if !is_default_profile(&self.name) => Some(&self.name),
            _ => None,
        }
    }

    /// Lists the persistent profiles (empty for self-tests).
    pub fn list_profiles(&self) -> Vec<String> {
        match (&self.roots, self.kind) {
            (Some(roots), ProfileKind::Normal | ProfileKind::Private) => roots.list(),
            _ => Vec::new(),
        }
    }

    /// Script installed in the chrome UI webview before its own scripts run:
    /// `window.__emberProfile = {name, private, badge, profiles}`.
    pub fn chrome_init_script(&self) -> String {
        let profiles: Vec<String> = if self.kind == ProfileKind::Normal {
            self.list_profiles()
        } else {
            Vec::new()
        };
        format!(
            "window.__emberProfile = {};",
            profile_info_json(self, &profiles)
        )
    }

    /// Script refreshing the profile list shown in the chrome menu after a create/delete.
    pub fn chrome_refresh_script(&self) -> String {
        let profiles = if self.kind == ProfileKind::Normal {
            self.list_profiles()
        } else {
            Vec::new()
        };
        format!(
            "window.__emberProfile = {}; if (typeof window.__emberApplyProfile === 'function') {{ window.__emberApplyProfile(); }}",
            profile_info_json(self, &profiles)
        )
    }
}

fn profile_info_json(ctx: &ProfileContext, profiles: &[String]) -> String {
    let mut out = String::from("{\"name\":");
    push_json_string(&mut out, &ctx.name);
    out.push_str(",\"private\":");
    out.push_str(if ctx.is_private() { "true" } else { "false" });
    out.push_str(",\"badge\":");
    match ctx.badge_text() {
        Some(badge) => push_json_string(&mut out, badge),
        None => out.push_str("null"),
    }
    out.push_str(",\"profiles\":[");
    for (idx, name) in profiles.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        push_json_string(&mut out, name);
    }
    out.push_str("]}");
    out
}

/// Appends `value` as a JSON / JavaScript string literal that is also safe inside `<script>`.
fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

static CURRENT: OnceLock<ProfileContext> = OnceLock::new();

/// Records the profile of this process (first call wins).
pub fn set_current(ctx: ProfileContext) {
    let _ = CURRENT.set(ctx);
}

/// Returns the profile of this process, if it has been set.
pub fn current() -> Option<&'static ProfileContext> {
    CURRENT.get()
}

/// Returns `true` if this process runs a private window.
pub fn is_private() -> bool {
    current().is_some_and(ProfileContext::is_private)
}

/// Window title base for this process (`Ember`, `Ember — Work`, `Ember — Private`).
pub fn window_title_base() -> String {
    current().map_or_else(|| String::from("Ember"), ProfileContext::window_title_base)
}

static FLASH: Mutex<Option<String>> = Mutex::new(None);

/// Stores a one-shot status message shown in the Profiles section of `ember://settings`.
pub fn set_flash(message: impl Into<String>) {
    if let Ok(mut guard) = FLASH.lock() {
        *guard = Some(message.into());
    }
}

/// Takes the pending status message, if any.
pub fn take_flash() -> Option<String> {
    FLASH.lock().ok().and_then(|mut guard| guard.take())
}

/// Exclusive lock on a profile / temporary directory, held for the life of the process
/// (released by the OS when the process exits, even after a crash).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Tries to take the exclusive lock of `dir` (creating `dir` if needed). Returns `Ok(None)` if
/// another process holds it.
pub fn try_lock_dir(dir: &Path) -> io::Result<Option<InstanceLock>> {
    fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// Returns this process's temporary directory for `prefix` (`ember_private_<pid>`, ...).
pub fn temp_profile_dir(prefix: &str) -> PathBuf {
    env::temp_dir().join(format!("{prefix}{}", std::process::id()))
}

/// Returns `true` if `path` is an Ember temporary profile directory directly inside the OS
/// temp dir (`<temp>/ember_private_<pid>` or `<temp>/ember_selftest_<pid>`).
pub fn is_ember_temp_dir(path: &Path) -> bool {
    let temp = env::temp_dir();
    let parent_ok = path.parent().is_some_and(|parent| same_path(parent, &temp));
    parent_ok
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_temp_dir_name)
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect::<Vec<_>>()
    };
    norm(a) == norm(b)
}

fn is_temp_dir_name(name: &str) -> bool {
    [PRIVATE_DIR_PREFIX, SELFTEST_DIR_PREFIX]
        .iter()
        .any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// Deletes stale private-window / self-test directories left in the temp dir by processes that
/// are gone (not holding their lock). Runs on a background thread.
pub fn sweep_stale_temp_dirs_async() {
    let _ = thread::Builder::new()
        .name(String::from("ember-temp-sweep"))
        .spawn(sweep_stale_temp_dirs);
}

fn sweep_stale_temp_dirs() {
    let Ok(entries) = fs::read_dir(env::temp_dir()) else {
        return;
    };
    let own_suffix = std::process::id().to_string();
    for entry in entries.filter_map(Result::ok) {
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        if !is_temp_dir_name(&name) || name.ends_with(&format!("_{own_suffix}")) {
            continue;
        }
        let young = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_none_or(|age| age < SWEEP_MIN_AGE);
        if young {
            continue;
        }
        let path = entry.path();
        if let Ok(Some(lock)) = try_lock_dir(&path) {
            drop(lock);
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Body of the `--cleanup-temp-dir=<dir>` helper: waits until the owning window process (and
/// its WebView2 processes) released the directory, then deletes it. Gives up after ~60s; the
/// startup sweep retries later.
pub fn run_cleanup_helper(dir: &Path) {
    if !is_ember_temp_dir(dir) {
        return;
    }
    for _ in 0..120 {
        if !dir.exists() {
            return;
        }
        if let Ok(Some(lock)) = try_lock_dir(dir) {
            drop(lock);
            if fs::remove_dir_all(dir).is_ok() {
                return;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn ember_command() -> io::Result<Command> {
    let mut cmd = Command::new(env::current_exe()?);
    cmd.stdin(Stdio::null());
    Ok(cmd)
}

/// Opens profile `name` in a new Ember process/window.
pub fn spawn_profile_window(name: &str) -> io::Result<()> {
    let mut cmd = ember_command()?;
    if !is_default_profile(name) {
        cmd.arg(format!("--profile={name}"));
    }
    cmd.spawn().map(|_| ())
}

/// Opens a private window (new process) that copies the settings of profile `source_profile`.
pub fn spawn_private_window(source_profile: &str) -> io::Result<()> {
    let mut cmd = ember_command()?;
    cmd.arg("--private");
    if !is_default_profile(source_profile) {
        cmd.arg(format!("--profile={source_profile}"));
    }
    cmd.spawn().map(|_| ())
}

/// Starts the detached helper that deletes `dir` once this process has exited.
pub fn spawn_cleanup_helper(dir: &Path) -> io::Result<()> {
    let mut cmd = ember_command()?;
    cmd.arg(format!("--cleanup-temp-dir={}", dir.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().map(|_| ())
}

/// Copies the adblock filter lists and serialized engine from `from` into `to` when `to` has
/// no engine yet, so a new profile or private window blocks ads immediately instead of
/// re-downloading ~17 MB of lists. The per-profile site allowlist is not copied.
pub fn seed_filters(from: &Path, to: &Path) {
    if same_path(from, to) || to.join("engine-v2.dat").exists() {
        return;
    }
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    if fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if name == "allowlist.txt" || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let _ = fs::copy(entry.path(), to.join(&name));
    }
}

/// Copies `settings.toml`, `theme.txt`, `newtab.txt` and the wallpaper video from a profile's config dir into a private window's
/// temporary dir so it looks and searches like the profile it was opened from.
pub fn seed_private_config(from_config_dir: &Path, to: &Path) {
    let _ = fs::create_dir_all(to);
    for file in [
        "settings.toml",
        "theme.txt",
        "newtab.txt",
        "wallpaper.mp4",
        "wallpaper.webm",
    ] {
        let src = from_config_dir.join(file);
        if src.is_file() {
            let _ = fs::copy(&src, to.join(file));
        }
    }
}

/// Returns `true` if site data may remain from a session that had "clear cookies and site data
/// when Ember closes" enabled but did not finish clearing (crash, kill, timeout).
pub fn site_data_clear_pending(data_dir: &Path) -> bool {
    data_dir.join(SITE_DATA_MARKER).exists()
}

/// Sets or removes the "site data must be cleared" marker in `data_dir`.
pub fn set_site_data_clear_pending(data_dir: &Path, pending: bool) {
    let marker = data_dir.join(SITE_DATA_MARKER);
    if pending {
        let _ = fs::write(marker, b"1");
    } else {
        let _ = fs::remove_file(marker);
    }
}

/// Renders the Profiles section of `ember://settings` (card contents).
pub fn settings_section_html(
    ctx: Option<&ProfileContext>,
    profiles: &[String],
    flash: Option<&str>,
) -> String {
    use crate::pages::escape_html;

    let mut out = String::new();
    if let Some(msg) = flash {
        out.push_str(&format!(
            r#"<div class="allow-empty" role="status">{}</div>"#,
            escape_html(msg)
        ));
    }
    let Some(ctx) = ctx else {
        out.push_str(r#"<div class="allow-empty">Profiles are not available.</div>"#);
        return out;
    };
    match ctx.kind {
        ProfileKind::SelfTest => {
            out.push_str(
                r#"<div class="allow-empty">Profiles are not available during the self-test.</div>"#,
            );
            return out;
        }
        ProfileKind::Private => {
            out.push_str(r#"<div class="allow-empty">This is a private window. Its history, cookies, site data and settings changes are deleted when it closes, and it does not offer to save passwords. Manage profiles from a normal window.</div>"#);
            return out;
        }
        ProfileKind::Normal => {}
    }
    out.push_str(r#"<div class="allow-empty">Each profile has its own settings, theme, history, bookmarks, cookies and saved passwords, and opens in its own window.</div>"#);
    for name in profiles {
        let is_current = name.eq_ignore_ascii_case(&ctx.name);
        let n = escape_html(name);
        let mut buttons = String::new();
        if is_current {
            buttons.push_str(r#"<span class="status">This window</span>"#);
        } else {
            buttons.push_str(&format!(
                r#"<button class="btn" data-name="{n}" onclick="act('profile_open', this.dataset.name)">Open</button>"#
            ));
            if !is_default_profile(name) {
                buttons.push_str(&format!(
                    r#" <button class="btn danger" data-name="{n}" onclick="deleteProfile(this.dataset.name)">Delete</button>"#
                ));
            }
        }
        out.push_str(&format!(
            r#"<div class="allow-item"><span>{n}</span><span>{buttons}</span></div>"#
        ));
    }
    out.push_str(r#"<div class="allow-add"><input id="profile-input" class="ctl" type="text" maxlength="32" placeholder="New profile name" spellcheck="false"><button class="btn" onclick="createProfile()">Add profile</button></div>"#);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_roots(tag: &str) -> ProfileRoots {
        let base = env::temp_dir().join(format!("ember_profile_test_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        ProfileRoots {
            config_root: base.join("config"),
            data_root: base.join("data"),
        }
    }

    fn cleanup(roots: &ProfileRoots) {
        if let Some(base) = roots.config_root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }

    #[test]
    fn test_validate_profile_name() {
        assert_eq!(validate_profile_name("Work").as_deref(), Ok("Work"));
        assert_eq!(
            validate_profile_name("  My Profile_2-b ").as_deref(),
            Ok("My Profile_2-b")
        );
        assert_eq!(
            validate_profile_name(&"a".repeat(32)).map(|s| s.len()),
            Ok(32)
        );
        assert_eq!(validate_profile_name(""), Err(ProfileNameError::Empty));
        assert_eq!(validate_profile_name("   "), Err(ProfileNameError::Empty));
        assert_eq!(
            validate_profile_name(&"a".repeat(33)),
            Err(ProfileNameError::TooLong)
        );
        for bad in [
            "../evil",
            "a/b",
            "a\\b",
            "C:",
            "dot.name",
            "tab\tname",
            "new\nline",
            "ünï",
            "a:b",
            "x*",
            "q?",
            "<x>",
            "\"q\"",
            "a|b",
            "%APPDATA%",
        ] {
            assert_eq!(
                validate_profile_name(bad),
                Err(ProfileNameError::InvalidCharacter),
                "{bad:?} must be rejected"
            );
        }
        for reserved in ["CON", "nul", "Aux", "prn", "COM1", "lpt9"] {
            assert_eq!(
                validate_profile_name(reserved),
                Err(ProfileNameError::Reserved)
            );
        }
        assert!(validate_profile_name("COM10").is_ok());
        assert!(validate_profile_name("Console").is_ok());
        assert!(is_default_profile("default"));
        assert!(is_default_profile(" Default "));
        assert!(!is_default_profile("Default2"));
    }

    #[test]
    fn test_parse_launch_args() {
        let args = parse_launch_args(["ember.exe", "https://a.test", "--profile=Work"])
            .expect("valid args");
        assert_eq!(args.profile.as_deref(), Some("Work"));
        assert!(!args.private);

        let args =
            parse_launch_args(["ember.exe", "--profile=default", "--private"]).expect("valid args");
        assert_eq!(args.profile, None);
        assert!(args.private);

        assert!(parse_launch_args(["ember.exe", "--profile=../x"]).is_err());
        assert!(parse_launch_args(["ember.exe", "--profile="]).is_err());
        assert_eq!(
            parse_launch_args(["ember.exe"]).expect("no args"),
            LaunchArgs::default()
        );
        let args =
            parse_launch_args(["x", "--cleanup-temp-dir=C:\\t\\ember_private_1"]).expect("cleanup");
        assert_eq!(
            args.cleanup_dir,
            Some(PathBuf::from("C:\\t\\ember_private_1"))
        );
    }

    #[test]
    fn test_profile_path_resolution() {
        let roots = ProfileRoots {
            config_root: PathBuf::from("/cfg/Ember"),
            data_root: PathBuf::from("/data/Ember"),
        };
        // Default keeps the historical locations.
        assert_eq!(
            roots.dirs_for("Default"),
            (PathBuf::from("/cfg/Ember"), PathBuf::from("/data/Ember"))
        );
        assert_eq!(roots.dirs_for("DEFAULT"), roots.dirs_for(DEFAULT_PROFILE));
        assert_eq!(
            roots.dirs_for("Work"),
            (
                PathBuf::from("/cfg/Ember").join("Profiles").join("Work"),
                PathBuf::from("/data/Ember").join("Profiles").join("Work")
            )
        );
    }

    #[test]
    fn test_create_list_delete_profiles() {
        let roots = test_roots("crud");
        assert_eq!(roots.list(), vec![String::from("Default")]);

        assert_eq!(roots.create("Work").as_deref(), Ok("Work"));
        assert_eq!(roots.create(" alpha ").as_deref(), Ok("alpha"));
        assert!(roots.create("work").is_err(), "case-insensitive duplicate");
        assert!(roots.create("Default").is_err());
        assert!(roots.create("bad/name").is_err());
        // Invalid directory names on disk are ignored.
        let _ = fs::create_dir_all(roots.config_root.join("Profiles").join("bad.name"));
        assert_eq!(roots.list(), vec!["Default", "alpha", "Work"]);
        assert_eq!(roots.find_existing("WORK").as_deref(), Some("Work"));
        assert_eq!(roots.canonical_name("wOrK"), "Work");
        assert_eq!(roots.canonical_name("New"), "New");

        let (cfg, data) = roots.dirs_for("Work");
        assert!(cfg.is_dir() && data.is_dir());

        assert!(roots.delete("Default", "Work").is_err());
        assert!(roots.delete("Work", "work").is_err(), "current profile");
        assert!(roots.delete("Missing", "Default").is_err());

        // A profile open in another process (lock held) cannot be deleted.
        let lock = try_lock_dir(&data)
            .expect("lock io")
            .expect("lock acquired");
        assert!(try_lock_dir(&data).expect("lock io").is_none());
        assert!(roots.delete("work", "Default").is_err());
        drop(lock);

        assert_eq!(roots.delete("work", "Default").as_deref(), Ok("Work"));
        assert!(!cfg.exists() && !data.exists());
        assert_eq!(roots.list(), vec!["Default", "alpha"]);
        cleanup(&roots);
    }

    #[test]
    fn test_temp_dir_names_and_context() {
        assert!(is_temp_dir_name("ember_private_123"));
        assert!(is_temp_dir_name("ember_selftest_9"));
        assert!(!is_temp_dir_name("ember_private_"));
        assert!(!is_temp_dir_name("ember_private_12a"));
        assert!(!is_temp_dir_name("other_123"));
        assert!(is_ember_temp_dir(&temp_profile_dir(PRIVATE_DIR_PREFIX)));
        assert!(!is_ember_temp_dir(Path::new(
            "C:\\Windows\\ember_private_1"
        )));
        assert!(!is_ember_temp_dir(
            &env::temp_dir().join("ember_private_1").join("x")
        ));

        let ctx = ProfileContext {
            name: String::from("Work"),
            kind: ProfileKind::Normal,
            config_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            roots: None,
        };
        assert_eq!(ctx.window_title_base(), "Ember \u{2014} Work");
        assert!(ctx.persists_session());
        let private = ProfileContext {
            kind: ProfileKind::Private,
            ..ctx.clone()
        };
        assert_eq!(private.window_title_base(), "Ember \u{2014} Private");
        assert!(!private.persists_session());
        assert!(private.chrome_init_script().contains("\"private\":true"));
        let default = ProfileContext {
            name: String::from("Default"),
            ..ctx.clone()
        };
        assert_eq!(default.window_title_base(), "Ember");
        assert!(default.chrome_init_script().contains("\"badge\":null"));
        let selftest = ProfileContext {
            kind: ProfileKind::SelfTest,
            ..ctx
        };
        assert!(!selftest.persists_session());
        assert_eq!(selftest.window_title_base(), "Ember");
    }

    #[test]
    fn test_settings_section_html() {
        let ctx = ProfileContext {
            name: String::from("Work"),
            kind: ProfileKind::Normal,
            config_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            roots: None,
        };
        let profiles = vec![
            String::from("Default"),
            String::from("Work"),
            String::from("Home"),
        ];
        let html = settings_section_html(Some(&ctx), &profiles, Some("<b>done</b>"));
        assert!(html.contains("&lt;b&gt;done&lt;/b&gt;"));
        assert!(html.contains("This window"));
        // Default can be opened but never deleted; Home can be deleted.
        assert_eq!(html.matches("profile_open").count(), 2);
        assert_eq!(html.matches("deleteProfile(").count(), 1);
        assert!(!html.contains("window.ipc"));
        let private = ProfileContext {
            kind: ProfileKind::Private,
            ..ctx
        };
        let html = settings_section_html(Some(&private), &profiles, None);
        assert!(!html.contains("profile_open"));
    }

    #[test]
    fn test_seed_filters_skips_allowlist_and_existing_engine() {
        let base = env::temp_dir().join(format!("ember_seed_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let from = base.join("from");
        let to = base.join("to");
        fs::create_dir_all(&from).expect("mkdir");
        fs::write(from.join("engine-v2.dat"), b"engine").expect("write");
        fs::write(from.join("easylist.txt"), b"list").expect("write");
        fs::write(from.join("allowlist.txt"), b"example.com").expect("write");
        seed_filters(&from, &to);
        assert!(to.join("engine-v2.dat").exists());
        assert!(to.join("easylist.txt").exists());
        assert!(!to.join("allowlist.txt").exists());
        fs::write(from.join("ublock.txt"), b"x").expect("write");
        seed_filters(&from, &to);
        assert!(!to.join("ublock.txt").exists(), "existing engine is kept");

        assert!(!site_data_clear_pending(&to));
        set_site_data_clear_pending(&to, true);
        assert!(site_data_clear_pending(&to));
        set_site_data_clear_pending(&to, false);
        assert!(!site_data_clear_pending(&to));
        let _ = fs::remove_dir_all(&base);
    }
}
