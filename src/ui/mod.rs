use tao::{
    dpi::PhysicalSize,
    window::{Theme, Window, WindowBuilder},
};
use wry::{
    dpi::{LogicalPosition, LogicalSize},
    Rect,
};

use crate::{
    settings::{SearchEngine, ThemePreference},
    tabs::TabSummary,
};

pub mod layout;

/// Microsoft's official Evergreen WebView2 Runtime bootstrapper download link.
pub const WEBVIEW2_INSTALLER_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// Fixed logical height of the top Chrome UI WebView (38px integrated titlebar/tab-strip + 46px toolbar).
pub const CHROME_HEIGHT: f64 = 84.0;

/// Embedded HTML/CSS/JS source for the top Chrome UI WebView.
pub const CHROME_HTML: &str = include_str!("chrome.html");

/// Initialization script injected into each tab's content WebView to report favicons,
/// scroll positions (for tab suspension/restore), and global browser keyboard shortcuts.
pub const PAGE_INIT_SCRIPT: &str = r#"
(function () {
  function post(msg) {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(msg);
    }
  }

  function detectFavicon() {
    try {
      var links = document.querySelectorAll(
        'link[rel~="icon"], link[rel="shortcut icon"], link[rel="apple-touch-icon"]'
      );
      for (var i = 0; i < links.length; i++) {
        var href = links[i].href;
        if (href && typeof href === "string" && href.length > 0) {
          post("favicon:" + href);
          return;
        }
      }
    } catch (_) {}
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", detectFavicon, { once: true });
  } else {
    detectFavicon();
  }
  window.addEventListener("load", detectFavicon);

  var scrollScheduled = false;
  window.addEventListener(
    "scroll",
    function () {
      if (!scrollScheduled) {
        scrollScheduled = true;
        window.requestAnimationFrame(function () {
          scrollScheduled = false;
          var x = Math.round(window.scrollX || window.pageXOffset || 0);
          var y = Math.round(window.scrollY || window.pageYOffset || 0);
          post("scroll:" + x + "," + y);
        });
      }
    },
    { passive: true }
  );

  window.addEventListener("keydown", function (e) {
    var key = (e.key || "").toLowerCase();
    var mod = e.ctrlKey || e.metaKey;

    if (e.altKey && !mod && !e.shiftKey) {
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        post("shortcut:back");
        return;
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        post("shortcut:forward");
        return;
      }
    }

    if (e.key === "F5") {
      e.preventDefault();
      post("shortcut:reload");
      return;
    }

    if (!mod) return;

    if (e.key === "Tab") {
      e.preventDefault();
      post(e.shiftKey ? "shortcut:prev_tab" : "shortcut:next_tab");
    } else if (e.shiftKey && key === "t") {
      e.preventDefault();
      post("shortcut:reopen_closed_tab");
    } else if (!e.shiftKey && key === "t") {
      e.preventDefault();
      post("shortcut:new_tab");
    } else if (!e.shiftKey && key === "w") {
      e.preventDefault();
      post("shortcut:close_tab");
    } else if (!e.shiftKey && key === "r") {
      e.preventDefault();
      post("shortcut:reload");
    } else if (!e.shiftKey && key === "l") {
      e.preventDefault();
      post("shortcut:focus_omnibox");
    } else if (!e.shiftKey && key === "d") {
      e.preventDefault();
      post("shortcut:toggle_bookmark");
    } else if (!e.shiftKey && key === "f") {
      e.preventDefault();
      post("shortcut:open_find");
    } else if (e.key === "+" || e.key === "=") {
      e.preventDefault();
      post("shortcut:zoom_in");
    } else if (e.key === "-" || e.key === "_") {
      e.preventDefault();
      post("shortcut:zoom_out");
    } else if (e.key === "0") {
      e.preventDefault();
      post("shortcut:zoom_reset");
    } else if (!e.shiftKey && e.key >= "1" && e.key <= "9") {
      e.preventDefault();
      post("shortcut:tab_index:" + e.key);
    }
  });
})();
"#;

/// Browser window and UI shell configuration.
#[derive(Debug, Clone)]
pub struct UiConfig {
    pub title: String,
    pub width: f64,
    pub height: f64,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            title: String::from("Ember"),
            width: 1180.0,
            height: 800.0,
        }
    }
}

impl UiConfig {
    /// Creates the default UI configuration for a browser window.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Windows 11 Snap Layout target zones within the current monitor's work area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapZone {
    LeftHalf,
    RightHalf,
    LeftTwoThirds,
    RightOneThird,
    LeftThird,
    CenterThird,
    RightThird,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl SnapZone {
    fn from_str(name: &str) -> Option<Self> {
        match name.trim() {
            "left_half" => Some(Self::LeftHalf),
            "right_half" => Some(Self::RightHalf),
            "left_two_thirds" => Some(Self::LeftTwoThirds),
            "right_one_third" => Some(Self::RightOneThird),
            "left_third" => Some(Self::LeftThird),
            "center_third" => Some(Self::CenterThird),
            "right_third" => Some(Self::RightThird),
            "top_left" => Some(Self::TopLeft),
            "top_right" => Some(Self::TopRight),
            "bottom_left" => Some(Self::BottomLeft),
            "bottom_right" => Some(Self::BottomRight),
            _ => None,
        }
    }

    /// Computes `(x, y, width, height)` inside a work area `(work_x, work_y, work_w, work_h)`.
    pub fn compute_rect(
        self,
        work_x: i32,
        work_y: i32,
        work_w: i32,
        work_h: i32,
    ) -> (i32, i32, i32, i32) {
        let half_w = work_w / 2;
        let half_h = work_h / 2;
        let third_w = work_w / 3;
        let two_thirds_w = (work_w * 2) / 3;

        match self {
            Self::LeftHalf => (work_x, work_y, half_w, work_h),
            Self::RightHalf => (work_x + half_w, work_y, work_w - half_w, work_h),
            Self::LeftTwoThirds => (work_x, work_y, two_thirds_w, work_h),
            Self::RightOneThird => (work_x + two_thirds_w, work_y, work_w - two_thirds_w, work_h),
            Self::LeftThird => (work_x, work_y, third_w, work_h),
            Self::CenterThird => (work_x + third_w, work_y, third_w, work_h),
            Self::RightThird => (work_x + third_w * 2, work_y, work_w - third_w * 2, work_h),
            Self::TopLeft => (work_x, work_y, half_w, half_h),
            Self::TopRight => (work_x + half_w, work_y, work_w - half_w, half_h),
            Self::BottomLeft => (work_x, work_y + half_h, half_w, work_h - half_h),
            Self::BottomRight => (
                work_x + half_w,
                work_y + half_h,
                work_w - half_w,
                work_h - half_h,
            ),
        }
    }
}

/// Snapshot of the window's theme, system accent color, and maximized state.
#[derive(Debug, Clone)]
pub struct WindowAppearance {
    pub is_dark: bool,
    pub accent_hex: String,
    pub is_maximized: bool,
}

/// Commands sent from the Chrome UI WebView to Rust via `window.ipc.postMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCommand {
    Ready,
    NewTab,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    SwitchTabIndex(usize),
    CloseTab(usize),
    DuplicateTab(usize),
    ReorderTab { dragged_id: usize, target_id: usize },
    SuspendTab(usize),
    ReopenClosedTab,
    Navigate(String),
    Back,
    Forward,
    Reload,
    Home,
    ToggleBookmark,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FindQuery(String),
    FindNext(String),
    FindPrev(String),
    FindClose,
    DownloadsClear,
    ToggleAdblock,
    ToggleSiteAllowlist,
    WindowDrag,
    WindowMinimize,
    WindowToggleMaximize,
    WindowClose,
    WindowSnap(SnapZone),
}

/// Parses a raw IPC message string sent from the top Chrome UI WebView.
pub fn parse_ipc_command(raw: &str) -> Option<UiCommand> {
    let trimmed = raw.trim();
    match trimmed {
        "ready" => Some(UiCommand::Ready),
        "new_tab" => Some(UiCommand::NewTab),
        "next_tab" => Some(UiCommand::NextTab),
        "prev_tab" => Some(UiCommand::PrevTab),
        "reopen_closed_tab" => Some(UiCommand::ReopenClosedTab),
        "back" => Some(UiCommand::Back),
        "forward" => Some(UiCommand::Forward),
        "reload" => Some(UiCommand::Reload),
        "home" => Some(UiCommand::Home),
        "toggle_bookmark" => Some(UiCommand::ToggleBookmark),
        "zoom_in" => Some(UiCommand::ZoomIn),
        "zoom_out" => Some(UiCommand::ZoomOut),
        "zoom_reset" => Some(UiCommand::ZoomReset),
        "find_close" => Some(UiCommand::FindClose),
        "downloads_clear" => Some(UiCommand::DownloadsClear),
        "toggle_adblock" => Some(UiCommand::ToggleAdblock),
        "toggle_site_allowlist" => Some(UiCommand::ToggleSiteAllowlist),
        "window_drag" => Some(UiCommand::WindowDrag),
        "window_minimize" => Some(UiCommand::WindowMinimize),
        "window_toggle_maximize" => Some(UiCommand::WindowToggleMaximize),
        "window_close" => Some(UiCommand::WindowClose),
        _ => {
            if let Some(id_str) = trimmed.strip_prefix("switch_tab:") {
                id_str
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .map(UiCommand::SwitchTab)
            } else if let Some(idx_str) = trimmed.strip_prefix("switch_tab_index:") {
                idx_str
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .map(UiCommand::SwitchTabIndex)
            } else if let Some(id_str) = trimmed.strip_prefix("close_tab:") {
                id_str.trim().parse::<usize>().ok().map(UiCommand::CloseTab)
            } else if let Some(id_str) = trimmed.strip_prefix("duplicate_tab:") {
                id_str
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .map(UiCommand::DuplicateTab)
            } else if let Some(id_str) = trimmed.strip_prefix("suspend_tab:") {
                id_str
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .map(UiCommand::SuspendTab)
            } else if let Some(pair_str) = trimmed.strip_prefix("reorder_tab:") {
                let mut parts = pair_str.split(':');
                let dragged_id = parts.next()?.trim().parse::<usize>().ok()?;
                let target_id = parts.next()?.trim().parse::<usize>().ok()?;
                Some(UiCommand::ReorderTab {
                    dragged_id,
                    target_id,
                })
            } else if let Some(snap_str) = trimmed.strip_prefix("window_snap:") {
                SnapZone::from_str(snap_str).map(UiCommand::WindowSnap)
            } else if let Some(q) = trimmed.strip_prefix("find_query:") {
                Some(UiCommand::FindQuery(q.to_owned()))
            } else if let Some(q) = trimmed.strip_prefix("find_next:") {
                Some(UiCommand::FindNext(q.to_owned()))
            } else if let Some(q) = trimmed.strip_prefix("find_prev:") {
                Some(UiCommand::FindPrev(q.to_owned()))
            } else {
                trimmed
                    .strip_prefix("navigate:")
                    .map(|target| UiCommand::Navigate(target.trim().to_owned()))
            }
        }
    }
}

/// Events sent from a tab's content WebView via `PAGE_INIT_SCRIPT`, `FIND_INIT_SCRIPT`,
/// `context_menu_init_script`, `ember://` internal pages, or `COSMETIC_DOCUMENT_CREATED_SCRIPT`.
#[derive(Debug, Clone, PartialEq)]
pub enum PageIpcEvent {
    Favicon(String),
    Scroll(f64, f64),
    CosmeticInit(String),
    CosmeticTokens {
        url: String,
        classes: Vec<String>,
        ids: Vec<String>,
    },
    FindResult {
        active: usize,
        total: usize,
    },
    ContextOpenNewTab(String),
    ContextCopyLink(String),
    ContextInspect,
    InternalNavigate(String),
    HistoryClear,
    HistoryDelete(String),
    BookmarkRemove(String),
    /// `ember://settings`: change a single setting (`key`, `value`).
    SettingsSet {
        key: String,
        value: String,
    },
    /// `ember://settings`: run an action such as `update_filters` or `clear_data`.
    SettingsAction {
        name: String,
        arg: String,
    },
    /// `ember://warning`: user chose to continue to an insecure HTTP site.
    HttpsProceed(String),
    ShortcutNewTab,
    ShortcutCloseTab,
    ShortcutReopenClosedTab,
    ShortcutNextTab,
    ShortcutPrevTab,
    ShortcutSwitchTabIndex(usize),
    ShortcutReload,
    ShortcutBack,
    ShortcutForward,
    ShortcutFocusOmnibox,
    ShortcutToggleBookmark,
    ShortcutOpenFind,
    ShortcutZoomIn,
    ShortcutZoomOut,
    ShortcutZoomReset,
}

/// Parses an IPC payload emitted inside a tab's page WebView.
pub fn parse_page_ipc_event(raw: &str) -> Option<PageIpcEvent> {
    let trimmed = raw.trim();
    match trimmed {
        "shortcut:new_tab" => Some(PageIpcEvent::ShortcutNewTab),
        "shortcut:close_tab" => Some(PageIpcEvent::ShortcutCloseTab),
        "shortcut:reopen_closed_tab" => Some(PageIpcEvent::ShortcutReopenClosedTab),
        "shortcut:next_tab" => Some(PageIpcEvent::ShortcutNextTab),
        "shortcut:prev_tab" => Some(PageIpcEvent::ShortcutPrevTab),
        "shortcut:reload" => Some(PageIpcEvent::ShortcutReload),
        "shortcut:back" => Some(PageIpcEvent::ShortcutBack),
        "shortcut:forward" => Some(PageIpcEvent::ShortcutForward),
        "shortcut:focus_omnibox" => Some(PageIpcEvent::ShortcutFocusOmnibox),
        "shortcut:toggle_bookmark" => Some(PageIpcEvent::ShortcutToggleBookmark),
        "shortcut:open_find" => Some(PageIpcEvent::ShortcutOpenFind),
        "shortcut:zoom_in" => Some(PageIpcEvent::ShortcutZoomIn),
        "shortcut:zoom_out" => Some(PageIpcEvent::ShortcutZoomOut),
        "shortcut:zoom_reset" => Some(PageIpcEvent::ShortcutZoomReset),
        "ctx_inspect:" => Some(PageIpcEvent::ContextInspect),
        "history_clear:" => Some(PageIpcEvent::HistoryClear),
        _ => {
            if let Some(idx_str) = trimmed.strip_prefix("shortcut:tab_index:") {
                idx_str
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .map(PageIpcEvent::ShortcutSwitchTabIndex)
            } else if let Some(fav) = trimmed.strip_prefix("favicon:") {
                let fav_trimmed = fav.trim();
                if !fav_trimmed.is_empty() {
                    Some(PageIpcEvent::Favicon(fav_trimmed.to_owned()))
                } else {
                    None
                }
            } else if let Some(coords) = trimmed.strip_prefix("scroll:") {
                let mut parts = coords.split(',');
                let x = parts.next()?.trim().parse::<f64>().ok()?;
                let y = parts.next()?.trim().parse::<f64>().ok()?;
                Some(PageIpcEvent::Scroll(x, y))
            } else if let Some(find_str) = trimmed.strip_prefix("find_result:") {
                let (active, total) = crate::find::parse_find_result(find_str)?;
                Some(PageIpcEvent::FindResult { active, total })
            } else if let Some(href) = trimmed.strip_prefix("ctx_open_new_tab:") {
                let h = href.trim();
                if !h.is_empty() {
                    Some(PageIpcEvent::ContextOpenNewTab(h.to_owned()))
                } else {
                    None
                }
            } else if let Some(href) = trimmed.strip_prefix("ctx_copy_link:") {
                let h = href.trim();
                if !h.is_empty() {
                    Some(PageIpcEvent::ContextCopyLink(h.to_owned()))
                } else {
                    None
                }
            } else if let Some(nav) = trimmed.strip_prefix("navigate:") {
                let n = nav.trim();
                if !n.is_empty() {
                    Some(PageIpcEvent::InternalNavigate(n.to_owned()))
                } else {
                    None
                }
            } else if let Some(del_url) = trimmed.strip_prefix("history_delete:") {
                Some(PageIpcEvent::HistoryDelete(del_url.trim().to_owned()))
            } else if let Some(rem_url) = trimmed.strip_prefix("bookmark_remove:") {
                Some(PageIpcEvent::BookmarkRemove(rem_url.trim().to_owned()))
            } else if let Some(kv) = trimmed.strip_prefix("settings_set:") {
                let (key, value) = kv.split_once('=')?;
                Some(PageIpcEvent::SettingsSet {
                    key: key.trim().to_owned(),
                    value: value.trim().to_owned(),
                })
            } else if let Some(action) = trimmed.strip_prefix("settings_action:") {
                let (name, arg) = action.split_once(':').unwrap_or((action, ""));
                Some(PageIpcEvent::SettingsAction {
                    name: name.trim().to_owned(),
                    arg: arg.trim().to_owned(),
                })
            } else if let Some(target) = trimmed.strip_prefix("https_proceed:") {
                Some(PageIpcEvent::HttpsProceed(target.trim().to_owned()))
            } else if let Some(url) = trimmed.strip_prefix("cosmetic_init:") {
                let url_trimmed = url.trim();
                if !url_trimmed.is_empty() {
                    Some(PageIpcEvent::CosmeticInit(url_trimmed.to_owned()))
                } else {
                    None
                }
            } else if let Some(json_str) = trimmed.strip_prefix("cosmetic_tokens:") {
                parse_cosmetic_tokens_payload(json_str)
            } else {
                None
            }
        }
    }
}

fn parse_cosmetic_tokens_payload(json_str: &str) -> Option<PageIpcEvent> {
    let url = extract_json_string_field(json_str, "\"url\"")?;
    let classes = extract_json_string_array(json_str, "\"classes\"");
    let ids = extract_json_string_array(json_str, "\"ids\"");
    Some(PageIpcEvent::CosmeticTokens { url, classes, ids })
}

fn extract_json_string_field(json: &str, key: &str) -> Option<String> {
    let idx = json.find(key)?;
    let after_key = &json[idx + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let content = &after_colon[1..];
    let mut out = String::new();
    let mut escaped = false;
    for ch in content.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

fn extract_json_string_array(json: &str, key: &str) -> Vec<String> {
    let Some(idx) = json.find(key) else {
        return Vec::new();
    };
    let after_key = &json[idx + key.len()..];
    let Some(open_bracket) = after_key.find('[') else {
        return Vec::new();
    };
    let after_bracket = &after_key[open_bracket + 1..];
    let Some(close_bracket) = after_bracket.find(']') else {
        return Vec::new();
    };
    let array_body = &after_bracket[..close_bracket];
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in array_body.chars() {
        if in_quotes {
            if escaped {
                current.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_quotes = false;
                if !current.is_empty() {
                    items.push(std::mem::take(&mut current));
                }
            } else {
                current.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
            current.clear();
        }
    }
    items
}

/// Configures the `WindowBuilder` for an integrated custom titlebar with native drop shadow.
pub fn configure_window_builder(builder: WindowBuilder) -> WindowBuilder {
    platform::configure_window_builder(builder.with_decorations(false))
}

/// Applies platform-native window frame attributes (Windows 11 rounded corners and immersive dark mode).
pub fn apply_window_chrome_style(window: &Window) {
    let is_dark = matches!(window.theme(), Theme::Dark);
    platform::apply_window_attributes(window, is_dark);
}

/// Queries the current OS theme, system accent color, and window maximized state.
pub fn query_window_appearance(window: &Window) -> WindowAppearance {
    let is_dark = matches!(window.theme(), Theme::Dark);
    let accent_hex = platform::system_accent_hex(is_dark);
    let is_maximized = window.is_maximized();

    WindowAppearance {
        is_dark,
        accent_hex,
        is_maximized,
    }
}

/// Snaps the window into the specified `SnapZone` of its current monitor's work area.
pub fn snap_window_to_zone(window: &Window, zone: SnapZone) {
    platform::snap_window_to_zone(window, zone);
}

/// Normalizes raw omnibox user input into a navigable URL or a search URL for `engine`.
pub fn normalize_omnibox_input(input: &str, engine: SearchEngine) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::from("ember://newtab");
    }

    if let Some(internal) = crate::pages::canonical_internal_url(trimmed) {
        return internal.to_owned();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("file://")
        || lower.starts_with("about:")
    {
        return trimmed.to_owned();
    }

    if lower == "localhost" || lower.starts_with("localhost:") || lower.starts_with("127.0.0.1") {
        return format!("http://{trimmed}");
    }

    let looks_like_host = !trimmed.contains(' ')
        && trimmed.contains('.')
        && !trimmed.starts_with('.')
        && !trimmed.ends_with('.');

    if looks_like_host {
        format!("https://{trimmed}")
    } else {
        let encoded = url_encode_query(trimmed);
        engine.build_search_url(&encoded)
    }
}

/// Applies the user's theme preference to the native window frame (title bar, caption controls).
pub fn apply_theme_preference(window: &Window, pref: ThemePreference) {
    window.set_theme(match pref {
        ThemePreference::System => None,
        ThemePreference::Light => Some(Theme::Light),
        ThemePreference::Dark => Some(Theme::Dark),
    });
    apply_window_chrome_style(window);
}

/// Applies the theme preference to a WebView so `prefers-color-scheme` follows it.
#[cfg(target_os = "windows")]
pub fn apply_webview_theme(webview: &wry::WebView, pref: ThemePreference) {
    use wry::WebViewExtWindows;
    let theme = match pref {
        ThemePreference::System => wry::Theme::Auto,
        ThemePreference::Light => wry::Theme::Light,
        ThemePreference::Dark => wry::Theme::Dark,
    };
    let _ = webview.set_theme(theme);
}

/// Applies the theme preference to a WebView (no-op on platforms without a per-view theme API).
#[cfg(not(target_os = "windows"))]
pub fn apply_webview_theme(_webview: &wry::WebView, _pref: ThemePreference) {}

fn url_encode_query(query: &str) -> String {
    let mut encoded = String::with_capacity(query.len());
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                encoded.push('%');
                let hi = byte >> 4;
                let lo = byte & 0x0F;
                encoded.push(hex_digit(hi));
                encoded.push(hex_digit(lo));
            }
        }
    }
    encoded
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Computes the `(chrome_bounds, content_bounds)` rectangles for the window's inner size.
pub fn compute_layout_bounds(inner_size: PhysicalSize<u32>, scale_factor: f64) -> (Rect, Rect) {
    let scale = if scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let logical_width = (f64::from(inner_size.width) / scale).max(1.0);
    let logical_height = (f64::from(inner_size.height) / scale).max(1.0);
    let chrome_height = CHROME_HEIGHT.min(logical_height);
    let content_height = (logical_height - chrome_height).max(1.0);

    let chrome_bounds = Rect {
        position: LogicalPosition::new(0.0, 0.0).into(),
        size: LogicalSize::new(logical_width, chrome_height).into(),
    };

    let content_bounds = Rect {
        position: LogicalPosition::new(0.0, chrome_height).into(),
        size: LogicalSize::new(logical_width, content_height).into(),
    };

    (chrome_bounds, content_bounds)
}

/// Snapshot of state synced from Rust to the top Chrome UI WebView.
pub struct ChromeSyncState<'a> {
    pub active_tab_id: usize,
    pub adblock_enabled: bool,
    pub active_site_allowlisted: bool,
    pub active_bookmarked: bool,
    pub active_zoom_percent: u32,
    pub find_open: bool,
    pub find_active: usize,
    pub find_total: usize,
    pub downloads_count: usize,
    pub downloads_active: usize,
    pub latest_download_text: &'a str,
    pub recently_closed_count: usize,
    pub tab_animations_enabled: bool,
    pub appearance: &'a WindowAppearance,
    pub tabs: &'a [TabSummary],
}

/// Builds a JavaScript invocation string that updates the Chrome UI WebView state.
pub fn build_state_sync_script(state: &ChromeSyncState<'_>) -> String {
    let mut script = String::with_capacity(480 + state.tabs.len() * 180);
    script.push_str("if(window.__emberUpdateState){window.__emberUpdateState({activeTabId:");
    script.push_str(&state.active_tab_id.to_string());
    script.push_str(",tabAnimationsEnabled:");
    script.push_str(if state.tab_animations_enabled {
        "true"
    } else {
        "false"
    });
    script.push_str(",adblockEnabled:");
    script.push_str(if state.adblock_enabled {
        "true"
    } else {
        "false"
    });
    script.push_str(",activeSiteAllowlisted:");
    script.push_str(if state.active_site_allowlisted {
        "true"
    } else {
        "false"
    });
    script.push_str(",activeBookmarked:");
    script.push_str(if state.active_bookmarked {
        "true"
    } else {
        "false"
    });
    script.push_str(",activeZoomPercent:");
    script.push_str(&state.active_zoom_percent.to_string());
    script.push_str(",findOpen:");
    script.push_str(if state.find_open { "true" } else { "false" });
    script.push_str(",findActive:");
    script.push_str(&state.find_active.to_string());
    script.push_str(",findTotal:");
    script.push_str(&state.find_total.to_string());
    script.push_str(",downloadsCount:");
    script.push_str(&state.downloads_count.to_string());
    script.push_str(",downloadsActive:");
    script.push_str(&state.downloads_active.to_string());
    script.push_str(",latestDownloadText:\"");
    push_js_escaped(&mut script, state.latest_download_text);
    script.push_str("\",recentlyClosedCount:");
    script.push_str(&state.recently_closed_count.to_string());
    script.push_str(",isMaximized:");
    script.push_str(if state.appearance.is_maximized {
        "true"
    } else {
        "false"
    });
    script.push_str(",theme:\"");
    script.push_str(if state.appearance.is_dark {
        "dark"
    } else {
        "light"
    });
    script.push_str("\",accentColor:\"");
    push_js_escaped(&mut script, &state.appearance.accent_hex);
    script.push_str("\",tabs:[");

    for (idx, tab) in state.tabs.iter().enumerate() {
        if idx > 0 {
            script.push(',');
        }
        script.push_str("{id:");
        script.push_str(&tab.id.to_string());
        script.push_str(",title:\"");
        push_js_escaped(&mut script, &tab.title);
        script.push_str("\",url:\"");
        push_js_escaped(&mut script, &tab.url);
        script.push_str("\",favicon:\"");
        if let Some(fav) = &tab.favicon_url {
            push_js_escaped(&mut script, fav);
        }
        script.push_str("\",loading:");
        script.push_str(if tab.is_loading { "true" } else { "false" });
        script.push_str(",suspended:");
        script.push_str(if tab.is_suspended { "true" } else { "false" });
        script.push_str(",blockedCount:");
        script.push_str(&tab.blocked_count.to_string());
        script.push('}');
    }

    script.push_str("]});}");
    script
}

fn push_js_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
}

/// Checks that the system WebView runtime (WebView2 on Windows) is installed at startup.
/// Returns a clear, user-facing error with a direct link to Microsoft's installer if missing.
pub fn ensure_webview_runtime() -> Result<String, String> {
    wry::webview_version().map_err(|err| {
        format!(
            "Microsoft Edge WebView2 Runtime is required to run Ember, but it was not found ({err}).\n\n\
             Please download and install the WebView2 Evergreen Runtime from Microsoft:\n\
             {WEBVIEW2_INSTALLER_URL}\n\
             (or visit https://developer.microsoft.com/en-us/microsoft-edge/webview2/)"
        )
    })
}

/// Displays a fatal startup error both to standard error and via an OS-native alert dialog when supported.
pub fn show_fatal_error(title: &str, message: &str) {
    eprintln!("{title}: {message}");
    platform::show_error_dialog(title, message);
}

#[cfg(target_os = "windows")]
mod platform {
    use core::ffi::c_void;
    use std::{mem::size_of, ptr};

    use tao::{
        platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
        window::{Window, WindowBuilder},
    };

    use super::SnapZone;

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;

    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;

    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;

    #[repr(C)]
    #[derive(Default)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: WinRect,
        rc_work: WinRect,
        dw_flags: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
        fn MonitorFromWindow(hwnd: isize, dw_flags: u32) -> isize;
        fn GetMonitorInfoW(hmonitor: isize, lpmi: *mut MonitorInfo) -> i32;
        fn SetWindowPos(
            hwnd: isize,
            hwnd_insert_after: isize,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            u_flags: u32,
        ) -> i32;
    }

    #[link(name = "dwmapi", kind = "raw-dylib")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            dw_attribute: u32,
            pv_attribute: *const c_void,
            cb_attribute: u32,
        ) -> i32;
        fn DwmGetColorizationColor(pcr_colorization: *mut u32, pf_opaque_blend: *mut i32) -> i32;
    }

    pub fn configure_window_builder(builder: WindowBuilder) -> WindowBuilder {
        builder.with_undecorated_shadow(true)
    }

    pub fn apply_window_attributes(window: &Window, is_dark: bool) {
        let hwnd = window.hwnd();
        let dark_flag: i32 = i32::from(is_dark);
        let corner_pref: u32 = DWMWCP_ROUND;

        // SAFETY: `hwnd` is a valid top-level window handle owned by `window`,
        // and the attribute pointers match the exact size expected by DWM on Windows 11.
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                ptr::addr_of!(dark_flag).cast::<c_void>(),
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                ptr::addr_of!(corner_pref).cast::<c_void>(),
                size_of::<u32>() as u32,
            );
        }
    }

    pub fn system_accent_hex(is_dark: bool) -> String {
        let mut colorization: u32 = 0;
        let mut opaque_blend: i32 = 0;

        // SAFETY: `colorization` and `opaque_blend` are valid stack locals passed to DWM.
        let hr = unsafe {
            DwmGetColorizationColor(
                ptr::addr_of_mut!(colorization),
                ptr::addr_of_mut!(opaque_blend),
            )
        };

        if hr == 0 {
            let r = ((colorization >> 16) & 0xFF) as u8;
            let g = ((colorization >> 8) & 0xFF) as u8;
            let b = (colorization & 0xFF) as u8;
            if r != 0 || g != 0 || b != 0 {
                return format!("#{r:02x}{g:02x}{b:02x}");
            }
        }

        if is_dark {
            String::from("#60cdff")
        } else {
            String::from("#0067c0")
        }
    }

    pub fn snap_window_to_zone(window: &Window, zone: SnapZone) {
        if window.is_maximized() {
            window.set_maximized(false);
        }

        let hwnd = window.hwnd();
        let mut info = MonitorInfo {
            cb_size: size_of::<MonitorInfo>() as u32,
            rc_monitor: WinRect::default(),
            rc_work: WinRect::default(),
            dw_flags: 0,
        };

        // SAFETY: `hwnd` is valid and `info.cb_size` is initialized to `size_of::<MonitorInfo>()`.
        let ok = unsafe {
            let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            hmon != 0 && GetMonitorInfoW(hmon, ptr::addr_of_mut!(info)) != 0
        };

        if ok {
            let work_x = info.rc_work.left;
            let work_y = info.rc_work.top;
            let work_w = (info.rc_work.right - info.rc_work.left).max(480);
            let work_h = (info.rc_work.bottom - info.rc_work.top).max(320);
            let (x, y, w, h) = zone.compute_rect(work_x, work_y, work_w, work_h);

            // SAFETY: Positions the top-level `hwnd` within the monitor's work area without altering Z-order.
            unsafe {
                let _ = SetWindowPos(hwnd, 0, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }

    pub fn show_error_dialog(title: &str, message: &str) {
        let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let message_wide: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: `title_wide` and `message_wide` are valid null-terminated UTF-16 buffers
        // that outlive the synchronous `MessageBoxW` call, and `hwnd` is null (no owner window).
        unsafe {
            MessageBoxW(
                ptr::null_mut(),
                message_wide.as_ptr(),
                title_wide.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use tao::{
        dpi::{PhysicalPosition, PhysicalSize},
        window::{Window, WindowBuilder},
    };

    use super::SnapZone;

    pub fn configure_window_builder(builder: WindowBuilder) -> WindowBuilder {
        builder
    }

    pub fn apply_window_attributes(_window: &Window, _is_dark: bool) {}

    pub fn system_accent_hex(is_dark: bool) -> String {
        if is_dark {
            String::from("#60cdff")
        } else {
            String::from("#0067c0")
        }
    }

    pub fn snap_window_to_zone(window: &Window, zone: SnapZone) {
        if window.is_maximized() {
            window.set_maximized(false);
        }

        if let Some(monitor) = window.current_monitor() {
            let pos = monitor.position();
            let size = monitor.size();
            let (x, y, w, h) =
                zone.compute_rect(pos.x, pos.y, size.width as i32, size.height as i32);
            window.set_outer_position(PhysicalPosition::new(x, y));
            window.set_inner_size(PhysicalSize::new(w.max(480) as u32, h.max(320) as u32));
        }
    }

    pub fn show_error_dialog(_title: &str, _message: &str) {
        // Non-Windows platforms log to stderr via `show_fatal_error`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SearchEngine;

    #[test]
    fn test_normalize_omnibox_input_urls() {
        assert_eq!(
            normalize_omnibox_input("example.com", SearchEngine::DuckDuckGo),
            "https://example.com"
        );
        assert_eq!(
            normalize_omnibox_input("https://rust-lang.org/learn", SearchEngine::Google),
            "https://rust-lang.org/learn"
        );
        assert_eq!(
            normalize_omnibox_input("ember://settings", SearchEngine::Brave),
            "ember://settings"
        );
        assert_eq!(
            normalize_omnibox_input("localhost:3000", SearchEngine::DuckDuckGo),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_omnibox_input("127.0.0.1:8080/test", SearchEngine::Startpage),
            "http://127.0.0.1:8080/test"
        );
    }

    #[test]
    fn test_normalize_omnibox_input_search_engines() {
        assert_eq!(
            normalize_omnibox_input("hello world", SearchEngine::DuckDuckGo),
            "https://duckduckgo.com/?q=hello+world"
        );
        assert_eq!(
            normalize_omnibox_input("rust language", SearchEngine::Google),
            "https://www.google.com/search?q=rust+language"
        );
        assert_eq!(
            normalize_omnibox_input("fast browser", SearchEngine::Brave),
            "https://search.brave.com/search?q=fast+browser"
        );
        assert_eq!(
            normalize_omnibox_input("windows 11", SearchEngine::Bing),
            "https://www.bing.com/search?q=windows+11"
        );
        assert_eq!(
            normalize_omnibox_input("privacy search", SearchEngine::Startpage),
            "https://www.startpage.com/sp/search?query=privacy+search"
        );
    }

    #[test]
    fn test_parse_settings_page_ipc_events() {
        assert_eq!(
            parse_page_ipc_event("settings_set:theme=dark"),
            Some(PageIpcEvent::SettingsSet {
                key: String::from("theme"),
                value: String::from("dark"),
            })
        );
        assert_eq!(
            parse_page_ipc_event("settings_action:clear_data:history,cookies"),
            Some(PageIpcEvent::SettingsAction {
                name: String::from("clear_data"),
                arg: String::from("history,cookies"),
            })
        );
        assert_eq!(
            parse_page_ipc_event("settings_action:update_filters"),
            Some(PageIpcEvent::SettingsAction {
                name: String::from("update_filters"),
                arg: String::new(),
            })
        );
        assert_eq!(
            parse_page_ipc_event("https_proceed:http://example.com/insecure"),
            Some(PageIpcEvent::HttpsProceed(String::from(
                "http://example.com/insecure"
            )))
        );
    }
}
