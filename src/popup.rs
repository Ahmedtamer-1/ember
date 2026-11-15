//! In-UI overlay controller for the omnibox suggestions dropdown and three-dot menu.
//!
//! Instead of spawning a separate borderless `tao::Window` popup (which suffers from
//! cross-window focus, Win32 visibility flag desync, and click-away issues), both overlays
//! render inside the top Chrome UI WebView (`chrome_webview`). Rust temporarily expands
//! `chrome_webview` over the full window above content WebViews when an overlay opens,
//! shrinks it back to [`ui::CHROME_HEIGHT`] when closed, and enforces a watchdog so the
//! UI WebView can never remain stuck expanded.

use std::time::{Duration, Instant};

use tao::window::Window;
use wry::WebView;

use crate::ui;

const WATCHDOG_HIDDEN_EXPANDED_TIMEOUT: Duration = Duration::from_millis(100);
const WATCHDOG_UNFOCUSED_OMNIBOX_TIMEOUT: Duration = Duration::from_millis(300);

fn popup_debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("EMBER_POPUP_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Logs overlay/omnibox diagnostic messages when `EMBER_POPUP_DEBUG=1`.
pub fn popup_debug_log(msg: &str) {
    if popup_debug_enabled() {
        eprintln!("[overlay] {msg}");
    }
}

/// Pure sequence ordering check for omnibox query responses.
///
/// Applies a response only if `query_id >= last_applied_id`, `query_id > cancelled_before_id`,
/// and `query_id == latest_sent_id`.
pub fn should_apply_omnibox_response(
    query_id: u64,
    latest_sent_id: u64,
    last_applied_id: u64,
    cancelled_before_id: u64,
) -> bool {
    query_id > 0
        && query_id == latest_sent_id
        && query_id >= last_applied_id
        && query_id > cancelled_before_id
}

/// Which in-UI overlay surface is currently expanded inside `chrome_webview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKind {
    Omnibox,
    Menu,
}

impl OverlayKind {
    /// Returns the wire string identifier (`"omnibox"` or `"menu"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Omnibox => "omnibox",
            Self::Menu => "menu",
        }
    }

    /// Parses an overlay kind string or legacy `open_popup` JSON payload.
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("omnibox") {
            return Some(Self::Omnibox);
        }
        if trimmed.eq_ignore_ascii_case("menu") {
            return Some(Self::Menu);
        }
        if trimmed.starts_with('{') {
            if let Some(kind_str) = extract_json_string(trimmed, "kind") {
                return Self::parse(&kind_str);
            }
        }
        None
    }
}

/// Tracks the expanded/collapsed state of the in-UI overlay inside `chrome_webview`
/// and runs the Rust-side watchdog that force-restores toolbar bounds if stuck.
#[derive(Debug)]
pub struct OverlayController {
    expanded: bool,
    dropdown_visible: bool,
    active_kind: Option<OverlayKind>,
    omnibox_focused: bool,
    omnibox_unfocused_since: Option<Instant>,
    hidden_while_expanded_since: Option<Instant>,
    last_applied_height: f64,
}

impl Default for OverlayController {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayController {
    /// Creates a new collapsed `OverlayController` starting at [`ui::CHROME_HEIGHT`].
    pub fn new() -> Self {
        Self {
            expanded: false,
            dropdown_visible: false,
            active_kind: None,
            omnibox_focused: false,
            omnibox_unfocused_since: None,
            hidden_while_expanded_since: None,
            last_applied_height: ui::CHROME_HEIGHT,
        }
    }

    /// Returns whether `chrome_webview` is currently expanded to cover the full window.
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Returns whether an overlay dropdown card is currently reported visible.
    pub fn is_dropdown_visible(&self) -> bool {
        self.dropdown_visible
    }

    /// Returns the currently active overlay kind, if any.
    pub fn active_kind(&self) -> Option<OverlayKind> {
        self.active_kind
    }

    /// Returns whether the omnibox input currently has focus.
    pub fn is_omnibox_focused(&self) -> bool {
        self.omnibox_focused
    }

    /// Returns the most recently applied logical height for `chrome_webview`.
    pub fn last_applied_height(&self) -> f64 {
        self.last_applied_height
    }

    /// Expands `chrome_webview` over the entire window above content WebViews.
    pub fn expand(&mut self, kind: OverlayKind, chrome_webview: &WebView, window: &Window) {
        ui::bring_chrome_webview_to_front(window);
        let full_bounds =
            ui::compute_expanded_chrome_bounds(window.inner_size(), window.scale_factor());
        let _ = chrome_webview.set_bounds(full_bounds);
        let scale = window.scale_factor().max(0.5);
        let full_height = full_bounds.size.to_logical::<f64>(scale).height;
        self.mark_expanded(kind, full_height, Instant::now());
        popup_debug_log(&format!(
            "expand kind={} height={full_height:.1}",
            kind.as_str()
        ));
    }

    /// Idempotently restores `chrome_webview` bounds back to [`ui::CHROME_HEIGHT`], even if called repeatedly.
    pub fn collapse(&mut self, reason: &str, chrome_webview: &WebView, window: &Window) {
        let (chrome_bounds, _) =
            ui::compute_layout_bounds(window.inner_size(), window.scale_factor());
        let _ = chrome_webview.set_bounds(chrome_bounds);
        let was_expanded = self.expanded;
        self.mark_collapsed();
        if was_expanded || popup_debug_enabled() {
            popup_debug_log(&format!(
                "collapse reason={reason} was_expanded={was_expanded} height={:.1}",
                ui::CHROME_HEIGHT
            ));
        }
    }

    /// Records pure state transitions when expanding the overlay (used by `expand` and unit tests).
    pub(crate) fn mark_expanded(&mut self, kind: OverlayKind, full_height: f64, now: Instant) {
        let was_expanded = self.expanded;
        self.expanded = true;
        self.dropdown_visible = true;
        self.active_kind = Some(kind);
        self.hidden_while_expanded_since = None;
        self.last_applied_height = full_height;
        if kind == OverlayKind::Omnibox
            && !self.omnibox_focused
            && (!was_expanded || self.omnibox_unfocused_since.is_none())
        {
            self.omnibox_unfocused_since = Some(now);
        }
    }

    /// Records pure state transitions when collapsing the overlay (used by `collapse` and unit tests).
    pub(crate) fn mark_collapsed(&mut self) {
        self.expanded = false;
        self.dropdown_visible = false;
        self.active_kind = None;
        self.hidden_while_expanded_since = None;
        self.omnibox_unfocused_since = None;
        self.last_applied_height = ui::CHROME_HEIGHT;
    }

    /// Marks the omnibox input as focused, cancelling any unfocused watchdog timer.
    pub fn on_omnibox_focus(&mut self) {
        self.omnibox_focused = true;
        self.omnibox_unfocused_since = None;
    }

    /// Marks the omnibox input as blurred at timestamp `now`.
    pub fn on_omnibox_blur(&mut self, now: Instant) {
        self.omnibox_focused = false;
        if self.omnibox_unfocused_since.is_none() {
            self.omnibox_unfocused_since = Some(now);
        }
    }

    /// Updates dropdown DOM visibility reported by `chrome.html`.
    pub fn on_dropdown_visibility(
        &mut self,
        visible: bool,
        kind: Option<OverlayKind>,
        now: Instant,
    ) {
        self.dropdown_visible = visible;
        self.active_kind = if visible { kind } else { None };
        if self.expanded && !visible {
            if self.hidden_while_expanded_since.is_none() {
                self.hidden_while_expanded_since = Some(now);
            }
        } else {
            self.hidden_while_expanded_since = None;
        }
    }

    /// Evaluates whether the expanded overlay has violated either watchdog invariant at time `now`.
    ///
    /// Triggers when:
    /// 1. The UI WebView is expanded while the dropdown is not visible (`dropdown_not_visible`), or
    /// 2. The omnibox dropdown is expanded while the omnibox has been unfocused for >= 300ms
    ///    (`omnibox_unfocused_300ms`).
    pub fn evaluate_watchdog(&self, now: Instant) -> Option<&'static str> {
        if !self.expanded {
            return None;
        }
        if !self.dropdown_visible
            && self.hidden_while_expanded_since.is_none_or(|t| {
                now.saturating_duration_since(t) >= WATCHDOG_HIDDEN_EXPANDED_TIMEOUT
            })
        {
            return Some("dropdown_not_visible");
        }
        if self.active_kind == Some(OverlayKind::Omnibox)
            && !self.omnibox_focused
            && self.omnibox_unfocused_since.is_some_and(|t| {
                now.saturating_duration_since(t) >= WATCHDOG_UNFOCUSED_OMNIBOX_TIMEOUT
            })
        {
            return Some("omnibox_unfocused_300ms");
        }
        None
    }

    /// Runs the watchdog check against real `chrome_webview` bounds, restoring toolbar height if stuck.
    pub fn check_watchdog(
        &mut self,
        now: Instant,
        chrome_webview: &WebView,
        window: &Window,
    ) -> bool {
        let Some(reason) = self.evaluate_watchdog(now) else {
            return false;
        };
        eprintln!(
            "[overlay-watchdog] WARNING: UI webview expanded invariant violated ({reason}); force-restoring bounds to toolbar height"
        );
        self.collapse(reason, chrome_webview, window);
        let _ = chrome_webview.evaluate_script(
            "if(window.closeSuggestions){window.closeSuggestions('rust_watchdog');}",
        );
        true
    }

    /// Returns `true` when the overlay state is collapsed and `chrome_webview`'s height equals toolbar height.
    pub fn is_at_toolbar_height(&self, chrome_webview: &WebView, window: &Window) -> bool {
        if self.expanded {
            return false;
        }
        let Ok(bounds) = chrome_webview.bounds() else {
            return (self.last_applied_height - ui::CHROME_HEIGHT).abs() <= 1.5;
        };
        let scale = window.scale_factor().max(0.5);
        let logical_size = bounds.size.to_logical::<f64>(scale);
        (logical_size.height - ui::CHROME_HEIGHT).abs() <= 1.5
    }
}

fn find_json_key_value_start<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let mut search_from = 0usize;
    while let Some(pos) = json[search_from..].find(&needle) {
        let abs_pos = search_from + pos + needle.len();
        let rest = json[abs_pos..].trim_start();
        if let Some(after_colon) = rest.strip_prefix(':') {
            return Some(after_colon.trim_start());
        }
        search_from = abs_pos;
    }
    None
}

pub(crate) fn extract_json_bool(json: &str, key: &str) -> Option<bool> {
    let val = find_json_key_value_start(json, key)?;
    if val.starts_with("true") {
        Some(true)
    } else if val.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let val = find_json_key_value_start(json, key)?;
    let inner = val.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_owned())
}

pub(crate) fn extract_json_number(json: &str, key: &str) -> Option<f64> {
    let val = find_json_key_value_start(json, key)?;
    let end = val
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != '+')
        .unwrap_or(val.len());
    if end == 0 {
        return None;
    }
    val[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overlay_kind_parse() {
        assert_eq!(OverlayKind::parse("omnibox"), Some(OverlayKind::Omnibox));
        assert_eq!(OverlayKind::parse(" MENU "), Some(OverlayKind::Menu));
        assert_eq!(
            OverlayKind::parse(r#"{"kind":"menu","anchor":{"left":0}}"#),
            Some(OverlayKind::Menu)
        );
        assert_eq!(OverlayKind::parse("none"), None);
    }

    #[test]
    fn test_overlay_controller_expand_collapse_idempotent() {
        let mut ctrl = OverlayController::new();
        assert!(!ctrl.is_expanded());
        assert_eq!(ctrl.last_applied_height(), ui::CHROME_HEIGHT);

        let t0 = Instant::now();
        ctrl.on_omnibox_focus();
        ctrl.mark_expanded(OverlayKind::Omnibox, 800.0, t0);
        assert!(ctrl.is_expanded());
        assert!(ctrl.is_dropdown_visible());
        assert_eq!(ctrl.active_kind(), Some(OverlayKind::Omnibox));
        assert_eq!(ctrl.last_applied_height(), 800.0);
        assert_eq!(ctrl.evaluate_watchdog(t0 + Duration::from_millis(50)), None);

        // Collapse twice must remain cleanly at toolbar height.
        ctrl.mark_collapsed();
        ctrl.mark_collapsed();
        assert!(!ctrl.is_expanded());
        assert!(!ctrl.is_dropdown_visible());
        assert_eq!(ctrl.active_kind(), None);
        assert_eq!(ctrl.last_applied_height(), ui::CHROME_HEIGHT);
        assert_eq!(ctrl.evaluate_watchdog(t0 + Duration::from_secs(5)), None);
    }

    #[test]
    fn test_watchdog_triggers_when_dropdown_hidden_while_expanded() {
        let mut ctrl = OverlayController::new();
        let t0 = Instant::now();
        ctrl.on_omnibox_focus();
        ctrl.mark_expanded(OverlayKind::Menu, 760.0, t0);
        assert_eq!(ctrl.evaluate_watchdog(t0), None);

        ctrl.on_dropdown_visibility(false, None, t0 + Duration::from_millis(10));
        // Still within 100ms grace window
        assert_eq!(ctrl.evaluate_watchdog(t0 + Duration::from_millis(90)), None);
        // Past 100ms while still expanded and dropdown hidden
        assert_eq!(
            ctrl.evaluate_watchdog(t0 + Duration::from_millis(120)),
            Some("dropdown_not_visible")
        );
    }

    #[test]
    fn test_watchdog_triggers_when_omnibox_unfocused_300ms() {
        let mut ctrl = OverlayController::new();
        let t0 = Instant::now();
        ctrl.on_omnibox_focus();
        ctrl.mark_expanded(OverlayKind::Omnibox, 800.0, t0);
        assert_eq!(
            ctrl.evaluate_watchdog(t0 + Duration::from_millis(500)),
            None
        );

        let blur_at = t0 + Duration::from_millis(600);
        ctrl.on_omnibox_blur(blur_at);
        assert_eq!(
            ctrl.evaluate_watchdog(blur_at + Duration::from_millis(299)),
            None
        );
        assert_eq!(
            ctrl.evaluate_watchdog(blur_at + Duration::from_millis(300)),
            Some("omnibox_unfocused_300ms")
        );

        // Refocusing clears the unfocused timer
        ctrl.on_omnibox_focus();
        assert_eq!(
            ctrl.evaluate_watchdog(blur_at + Duration::from_secs(2)),
            None
        );

        // Stale blur from an old session while collapsed must not insta-kill a fresh expand
        ctrl.on_omnibox_blur(t0);
        ctrl.mark_collapsed();
        let fresh_expand = t0 + Duration::from_secs(10);
        ctrl.mark_expanded(OverlayKind::Omnibox, 800.0, fresh_expand);
        assert_eq!(
            ctrl.evaluate_watchdog(fresh_expand + Duration::from_millis(50)),
            None
        );
        assert_eq!(
            ctrl.evaluate_watchdog(fresh_expand + Duration::from_millis(300)),
            Some("omnibox_unfocused_300ms")
        );
    }

    #[test]
    fn test_omnibox_sequence_filter() {
        assert!(should_apply_omnibox_response(1, 1, 0, 0));
        assert!(should_apply_omnibox_response(2, 2, 1, 0));
        assert!(!should_apply_omnibox_response(1, 3, 2, 0));
        assert!(!should_apply_omnibox_response(3, 3, 2, 3));
        assert!(should_apply_omnibox_response(4, 4, 2, 3));
    }
}
