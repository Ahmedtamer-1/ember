//! Reusable borderless popup overlay infrastructure (`Menu`, `Omnibox`, etc.).
//!
//! Uses a lazily-created, opaque, owner-attached `tao::Window` + `wry::WebView` with
//! Windows 11 DWM rounded corners, themed border/background, and paint-synchronized reveal
//! so dropdowns float cleanly above sibling WebView2 content HWNDs with zero border artifacts
//! or open flash.

use std::time::{Duration, Instant};

use tao::{
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize},
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    window::{Window, WindowBuilder, WindowId},
};
use wry::{Rect, WebContext, WebView, WebViewBuilder, RGBA};

use crate::{
    app::AppEvent,
    ui::{self, WindowAppearance},
};

const MENU_CARD_WIDTH: f64 = 276.0;
const MENU_CARD_HEIGHT: f64 = 378.0;

fn popup_bg_rgba(is_dark: bool) -> RGBA {
    if is_dark {
        (41, 42, 45, 255) // #292a2d
    } else {
        (255, 255, 255, 255) // #ffffff
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    Menu,
    Omnibox,
}

impl PopupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Omnibox => "omnibox",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "menu" => Some(Self::Menu),
            "omnibox" => Some(Self::Omnibox),
            _ => None,
        }
    }

    pub fn needs_focus(self) -> bool {
        matches!(self, Self::Menu)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnchorRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone)]
pub struct OpenPopupRequest {
    pub kind: PopupKind,
    pub anchor_id: String,
    pub toggle: bool,
    pub anchor: AnchorRect,
    pub data_json: String,
}

/// Single source of truth for the popup lifecycle.
fn popup_debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("EMBER_POPUP_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn popup_debug_log(msg: &str) {
    if popup_debug_enabled() {
        eprintln!("[popup-sm] {msg}");
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupState {
    Closed,
    Opening { kind: PopupKind, anchor_id: String },
    Open { kind: PopupKind, anchor_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    BlurOrOutsideClick,
    Explicit,
    SystemEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenDecision {
    StartOpening { generation: u64 },
    UpdateInPlace { generation: u64 },
    UpdateWhileOpening { generation: u64 },
    ClosedInstead { close_gen: u64 },
    SuppressedToggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseAction {
    pub generation: u64,
    pub was_opening: bool,
}

/// Pure, window-free state machine governing popup open, toggle-suppression, and close transitions.
#[derive(Debug)]
pub struct PopupStateMachine {
    state: PopupState,
    generation: u64,
    last_blur_close: Option<(String, Instant)>,
}

const TOGGLE_SUPPRESS_WINDOW: Duration = Duration::from_millis(250);

impl Default for PopupStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupStateMachine {
    pub fn new() -> Self {
        Self {
            state: PopupState::Closed,
            generation: 0,
            last_blur_close: None,
        }
    }

    pub fn state(&self) -> &PopupState {
        &self.state
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.state, PopupState::Closed)
    }

    pub fn current_kind(&self) -> Option<PopupKind> {
        match &self.state {
            PopupState::Closed => None,
            PopupState::Opening { kind, .. } | PopupState::Open { kind, .. } => Some(*kind),
        }
    }

    pub fn request_open(
        &mut self,
        kind: PopupKind,
        anchor_id: &str,
        toggle: bool,
        now: Instant,
    ) -> OpenDecision {
        if toggle {
            match &self.state {
                PopupState::Opening {
                    kind: cur_kind,
                    anchor_id: cur_anchor,
                }
                | PopupState::Open {
                    kind: cur_kind,
                    anchor_id: cur_anchor,
                } if *cur_kind == kind && cur_anchor == anchor_id => {
                    let close_gen = self
                        .request_close(CloseReason::Explicit, now)
                        .map_or(self.generation, |c| c.generation);
                    return OpenDecision::ClosedInstead { close_gen };
                }
                PopupState::Closed => {
                    if let Some((last_anchor, closed_at)) = &self.last_blur_close {
                        if last_anchor == anchor_id
                            && now.saturating_duration_since(*closed_at) <= TOGGLE_SUPPRESS_WINDOW
                        {
                            self.last_blur_close = None;
                            return OpenDecision::SuppressedToggle;
                        }
                    }
                }
                _ => {}
            }
        }

        self.last_blur_close = None;

        if !toggle {
            match &mut self.state {
                PopupState::Open {
                    kind: cur_kind,
                    anchor_id: cur_anchor,
                } if *cur_kind == kind => {
                    *cur_anchor = anchor_id.to_owned();
                    return OpenDecision::UpdateInPlace {
                        generation: self.generation,
                    };
                }
                PopupState::Opening {
                    kind: cur_kind,
                    anchor_id: cur_anchor,
                } if *cur_kind == kind => {
                    *cur_anchor = anchor_id.to_owned();
                    return OpenDecision::UpdateWhileOpening {
                        generation: self.generation,
                    };
                }
                _ => {}
            }
        }

        self.generation += 1;
        popup_debug_log(&format!(
            "transition {:?} -> Opening({:?}, anchor={}) gen={}",
            self.state, kind, anchor_id, self.generation
        ));
        self.state = PopupState::Opening {
            kind,
            anchor_id: anchor_id.to_owned(),
        };
        OpenDecision::StartOpening {
            generation: self.generation,
        }
    }

    pub fn on_ready_to_show(&mut self, gen: u64) -> Option<PopupKind> {
        if gen != self.generation {
            popup_debug_log(&format!(
                "ignored stale ready_to_show gen={} current={}",
                gen, self.generation
            ));
            return None;
        }
        if let PopupState::Opening { kind, anchor_id } = &self.state {
            let kind = *kind;
            popup_debug_log(&format!(
                "transition Opening -> Open({:?}, anchor={}) gen={}",
                kind, anchor_id, gen
            ));
            self.state = PopupState::Open {
                kind,
                anchor_id: anchor_id.clone(),
            };
            Some(kind)
        } else {
            None
        }
    }

    pub fn request_close(&mut self, reason: CloseReason, now: Instant) -> Option<CloseAction> {
        let (was_opening, anchor_id) = match &self.state {
            PopupState::Closed => return None,
            PopupState::Opening { anchor_id, .. } => (true, anchor_id.clone()),
            PopupState::Open { anchor_id, .. } => (false, anchor_id.clone()),
        };

        if reason == CloseReason::BlurOrOutsideClick {
            self.last_blur_close = Some((anchor_id, now));
        } else {
            self.last_blur_close = None;
        }

        self.generation += 1;
        popup_debug_log(&format!(
            "transition {:?} -> Closed reason={:?} gen={}",
            self.state, reason, self.generation
        ));
        self.state = PopupState::Closed;
        Some(CloseAction {
            generation: self.generation,
            was_opening,
        })
    }
}

pub(crate) struct PopupOpenContext<'a> {
    pub event_loop: &'a EventLoopWindowTarget<AppEvent>,
    pub main_window: &'a Window,
    pub web_context: &'a mut WebContext,
    pub proxy: &'a EventLoopProxy<AppEvent>,
    pub appearance: &'a WindowAppearance,
    pub reduced_motion: bool,
}

/// Manages the single lazily-allocated popup overlay window and its WebView.
pub struct PopupManager {
    window: Option<Window>,
    webview: Option<WebView>,
    sm: PopupStateMachine,
    is_ready: bool,
    pending_open_script: Option<String>,
}

impl Default for PopupManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupManager {
    pub fn new() -> Self {
        Self {
            window: None,
            webview: None,
            sm: PopupStateMachine::new(),
            is_ready: false,
            pending_open_script: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sm.is_open()
    }

    pub fn current_kind(&self) -> Option<PopupKind> {
        self.sm.current_kind()
    }

    pub fn is_popup_window(&self, id: WindowId) -> bool {
        self.window.as_ref().is_some_and(|w| w.id() == id)
    }

    /// Returns `true` if the OS foreground window is either `main_window` or our popup window.
    pub fn is_foreground_ours(&self, main_window: &Window) -> bool {
        platform::is_foreground_either(main_window, self.window.as_ref())
    }

    /// Lazily initializes the popup window and WebView on first use, sharing `web_context`
    /// so no extra WebView2 browser process is spawned.
    fn ensure_created(
        &mut self,
        event_loop: &EventLoopWindowTarget<AppEvent>,
        main_window: &Window,
        web_context: &mut WebContext,
        proxy: &EventLoopProxy<AppEvent>,
        is_dark: bool,
    ) -> Result<(), wry::Error> {
        if self.window.is_some() && self.webview.is_some() {
            return Ok(());
        }

        let mut builder = WindowBuilder::new()
            .with_decorations(false)
            .with_visible(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(MENU_CARD_WIDTH, MENU_CARD_HEIGHT));

        builder = platform::configure_popup_window_builder(builder, main_window);

        let popup_win = builder
            .build(event_loop)
            .map_err(|_| wry::Error::InitScriptError)?;

        platform::apply_popup_dwm_attributes(&popup_win, is_dark);

        let ipc_proxy = proxy.clone();
        let initial_bounds = Rect {
            position: LogicalPosition::new(0.0, 0.0).into(),
            size: LogicalSize::new(MENU_CARD_WIDTH, MENU_CARD_HEIGHT).into(),
        };

        let popup_wv = WebViewBuilder::with_web_context(web_context)
            .with_bounds(initial_bounds)
            .with_background_color(popup_bg_rgba(is_dark))
            .with_html(POPUP_HTML)
            .with_ipc_handler(move |request| {
                let _ = ipc_proxy.send_event(AppEvent::PopupIpc(request.body().clone()));
            })
            .build_as_child(&popup_win)?;

        let theme_pref = if is_dark {
            crate::settings::ThemePreference::Dark
        } else {
            crate::settings::ThemePreference::Light
        };
        ui::apply_webview_theme(&popup_wv, theme_pref);

        self.window = Some(popup_win);
        self.webview = Some(popup_wv);
        self.is_ready = false;
        Ok(())
    }

    /// Opens, toggles, or updates the popup overlay anchored to `req.anchor` in `main_window` coordinates.
    pub(crate) fn open(
        &mut self,
        ctx: PopupOpenContext<'_>,
        req: &OpenPopupRequest,
    ) -> Result<(), wry::Error> {
        if req.kind == PopupKind::Omnibox {
            let count = extract_json_number(&req.data_json, "count").unwrap_or(0.0);
            if count <= 0.0 {
                self.close(true, CloseReason::Explicit, Some(ctx.proxy));
                return Ok(());
            }
        }

        let decision = self
            .sm
            .request_open(req.kind, &req.anchor_id, req.toggle, Instant::now());
        let is_in_place_update = matches!(
            decision,
            OpenDecision::UpdateInPlace { .. } | OpenDecision::UpdateWhileOpening { .. }
        );
        let gen = match decision {
            OpenDecision::SuppressedToggle => return Ok(()),
            OpenDecision::ClosedInstead { close_gen } => {
                self.begin_close_animation(close_gen, false, Some(ctx.proxy));
                return Ok(());
            }
            OpenDecision::StartOpening { generation }
            | OpenDecision::UpdateInPlace { generation }
            | OpenDecision::UpdateWhileOpening { generation } => generation,
        };

        self.ensure_created(
            ctx.event_loop,
            ctx.main_window,
            ctx.web_context,
            ctx.proxy,
            ctx.appearance.is_dark,
        )?;

        let main_window = ctx.main_window;
        let appearance = ctx.appearance;
        let reduced_motion = ctx.reduced_motion;

        let scale = main_window.scale_factor().max(0.5);
        let main_inner_pos = main_window
            .inner_position()
            .unwrap_or_else(|_| PhysicalPosition::new(0, 0));

        let (card_w, card_h) = match req.kind {
            PopupKind::Menu => (MENU_CARD_WIDTH, MENU_CARD_HEIGHT),
            PopupKind::Omnibox => {
                let count = extract_json_number(&req.data_json, "count").unwrap_or(1.0);
                let h = count.clamp(1.0, 8.0) * 38.0 + 12.0;
                (req.anchor.width.max(320.0), h)
            }
        };

        // Position in logical coordinates relative to main window's client origin:
        // Menu aligns its right edge with anchor.right; Omnibox aligns left edge.
        let card_left_logical = match req.kind {
            PopupKind::Menu => req.anchor.right - card_w,
            PopupKind::Omnibox => req.anchor.left,
        };
        let card_top_logical = req.anchor.bottom + 4.0;

        let mut phys_x = main_inner_pos.x + (card_left_logical * scale).round() as i32;
        let mut phys_y = main_inner_pos.y + (card_top_logical * scale).round() as i32;
        let phys_w = (card_w * scale).round().max(1.0) as u32;
        let phys_h = (card_h * scale).round().max(1.0) as u32;

        // Clamp to the current monitor's work area so the menu never clips off-screen.
        if let Some(monitor) = main_window.current_monitor() {
            let mon_pos = monitor.position();
            let mon_size = monitor.size();
            let min_x = mon_pos.x;
            let min_y = mon_pos.y;
            let max_x = mon_pos.x + mon_size.width as i32 - phys_w as i32;
            let max_y = mon_pos.y + mon_size.height as i32 - phys_h as i32;
            if max_x >= min_x {
                phys_x = phys_x.clamp(min_x, max_x);
            }
            if max_y >= min_y {
                phys_y = phys_y.clamp(min_y, max_y);
            }
        }

        if let (Some(win), Some(wv)) = (&self.window, &self.webview) {
            let theme_pref = if appearance.is_dark {
                crate::settings::ThemePreference::Dark
            } else {
                crate::settings::ThemePreference::Light
            };
            ui::apply_webview_theme(wv, theme_pref);
            let _ = wv.set_background_color(popup_bg_rgba(appearance.is_dark));
            platform::apply_popup_dwm_attributes(win, appearance.is_dark);
            platform::set_popup_no_activate(win, !req.kind.needs_focus());

            win.set_outer_position(PhysicalPosition::new(phys_x, phys_y));
            win.set_inner_size(PhysicalSize::new(phys_w, phys_h));
            let _ = wv.set_bounds(Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                size: LogicalSize::new(card_w, card_h).into(),
            });

            let data_payload = if req.data_json.trim().is_empty() {
                "{}"
            } else {
                req.data_json.as_str()
            };
            let script = if is_in_place_update && self.is_ready && req.kind == PopupKind::Omnibox {
                format!(
                    "if(window.__emberUpdatePopupData){{window.__emberUpdatePopupData({data_payload});}}"
                )
            } else {
                format!(
                    "if(window.__emberOpenPopup){{window.__emberOpenPopup({kind},{data},{dark},{reduced},{gen});}}",
                    kind = serde_json_str(req.kind.as_str()),
                    data = data_payload,
                    dark = if appearance.is_dark { "true" } else { "false" },
                    reduced = if reduced_motion { "true" } else { "false" },
                )
            };

            if self.is_ready {
                self.pending_open_script = None;
                let _ = wv.evaluate_script(&script);
            } else {
                self.pending_open_script = Some(script);
            }
        }

        Ok(())
    }

    /// Called when `POPUP_HTML` finishes initial script setup and can accept `__emberOpenPopup`.
    pub fn on_init_ready(&mut self) {
        self.is_ready = true;
        if let (Some(script), Some(wv)) = (self.pending_open_script.take(), &self.webview) {
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Called after the popup DOM has rendered and painted its first frame, revealing the window.
    pub fn on_ready_to_show(&mut self, gen: u64) {
        let Some(kind) = self.sm.on_ready_to_show(gen) else {
            return;
        };
        if let Some(win) = &self.window {
            if kind.needs_focus() {
                win.set_visible(true);
                win.set_focus();
            } else {
                platform::show_window_no_activate(win);
            }
        }
    }

    /// Updates live popup data (such as zoom percent) while keeping the popup open.
    pub fn update_data(&self, data_json: &str) {
        if !self.is_open() {
            return;
        }
        if let Some(wv) = &self.webview {
            let script = format!(
                "if(window.__emberUpdatePopupData){{window.__emberUpdatePopupData({data_json});}}"
            );
            let _ = wv.evaluate_script(&script);
        }
    }

    /// Begins closing the popup idempotently. When `animate` is true and the popup is visible,
    /// plays the 120ms exit animation with a 160ms Rust fallback timer so the window always hides.
    pub(crate) fn close(
        &mut self,
        animate: bool,
        reason: CloseReason,
        proxy: Option<&EventLoopProxy<AppEvent>>,
    ) {
        let Some(action) = self.sm.request_close(reason, Instant::now()) else {
            return;
        };
        self.pending_open_script = None;
        if animate && !action.was_opening {
            self.begin_close_animation(action.generation, false, proxy);
        } else if let Some(win) = &self.window {
            win.set_visible(false);
        }
    }

    fn begin_close_animation(
        &self,
        close_gen: u64,
        _was_opening: bool,
        proxy: Option<&EventLoopProxy<AppEvent>>,
    ) {
        if let Some(wv) = &self.webview {
            let script = format!(
                "if(window.__emberClosePopup){{window.__emberClosePopup({close_gen});}}else if(window.ipc){{window.ipc.postMessage('popup_closed:{close_gen}');}}"
            );
            let _ = wv.evaluate_script(&script);
            if let Some(proxy) = proxy {
                let timeout_proxy = proxy.clone();
                let _ = std::thread::Builder::new()
                    .name(String::from("ember-popup-close-timeout"))
                    .spawn(move || {
                        std::thread::sleep(Duration::from_millis(160));
                        let _ = timeout_proxy.send_event(AppEvent::PopupCloseTimeout(close_gen));
                    });
            }
            return;
        }
        if let Some(win) = &self.window {
            win.set_visible(false);
        }
    }

    /// Completes closing when `"popup_closed:<gen>"` or `PopupCloseTimeout(gen)` arrives.
    pub fn finish_close(&self, close_gen: u64) {
        if close_gen == self.sm.generation() && !self.sm.is_open() {
            if let Some(win) = &self.window {
                win.set_visible(false);
            }
            self.assert_closed_invariant("finish_close");
        }
    }

    /// Immediately hides the popup window without animation.
    pub fn hide_now(&mut self) {
        let _ = self
            .sm
            .request_close(CloseReason::SystemEvent, Instant::now());
        self.pending_open_script = None;
        if let Some(win) = &self.window {
            win.set_visible(false);
        }
        self.assert_closed_invariant("hide_now");
    }

    /// Verifies that no visible orphan popup window remains when the state machine is `Closed`.
    pub fn assert_closed_invariant(&self, context: &str) {
        if self.sm.state() == &PopupState::Closed {
            if let Some(win) = &self.window {
                if win.is_visible() {
                    eprintln!(
                        "[popup-sm] WARNING: orphan visible popup detected at {context}; forcing hidden"
                    );
                    win.set_visible(false);
                    debug_assert!(
                        !win.is_visible(),
                        "popup window remained visible after Closed at {context}"
                    );
                }
            }
        }
    }
}

fn serde_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parses an `open_popup:<json>` payload from `chrome.html`.
/// Expects minimal flat JSON: `{"kind":"menu","anchor":{"left":...,"top":...,"right":...,"bottom":...,"width":...,"height":...},"data":{...}}`
pub fn parse_open_popup_payload(json_str: &str) -> Option<OpenPopupRequest> {
    let kind_str = extract_json_string(json_str, "kind")?;
    let kind = PopupKind::parse(&kind_str)?;
    let anchor_id = extract_json_string(json_str, "anchorId")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| kind.as_str().to_owned());
    let toggle = extract_json_bool(json_str, "toggle").unwrap_or(false);

    let anchor_obj = extract_json_object(json_str, "anchor")?;
    let anchor = AnchorRect {
        left: extract_json_number(anchor_obj, "left")?,
        top: extract_json_number(anchor_obj, "top")?,
        right: extract_json_number(anchor_obj, "right")?,
        bottom: extract_json_number(anchor_obj, "bottom")?,
        width: extract_json_number(anchor_obj, "width").unwrap_or(0.0),
        height: extract_json_number(anchor_obj, "height").unwrap_or(0.0),
    };

    let data_json = extract_json_object(json_str, "data")
        .unwrap_or("{}")
        .to_owned();

    Some(OpenPopupRequest {
        kind,
        anchor_id,
        toggle,
        anchor,
        data_json,
    })
}

fn extract_json_bool(json: &str, key: &str) -> Option<bool> {
    let val = find_json_key_value_start(json, key)?;
    if val.starts_with("true") {
        Some(true)
    } else if val.starts_with("false") {
        Some(false)
    } else {
        None
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

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let val = find_json_key_value_start(json, key)?;
    let inner = val.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_owned())
}

fn extract_json_number(json: &str, key: &str) -> Option<f64> {
    let val = find_json_key_value_start(json, key)?;
    let end = val
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != '+')
        .unwrap_or(val.len());
    if end == 0 {
        return None;
    }
    val[..end].parse::<f64>().ok()
}

fn extract_json_object<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let val = find_json_key_value_start(json, key)?;
    if !val.starts_with('{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (idx, ch) in val.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&val[..=idx]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(target_os = "windows")]
mod platform {
    use core::ffi::c_void;
    use core::mem::size_of;
    use core::ptr;

    use tao::{
        platform::windows::{WindowBuilderExtWindows, WindowExtWindows},
        window::{Window, WindowBuilder},
    };

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn ShowWindow(hwnd: isize, cmd_show: i32) -> i32;
        fn GetWindowLongW(hwnd: isize, n_index: i32) -> i32;
        fn SetWindowLongW(hwnd: isize, n_index: i32, dw_new_long: i32) -> i32;
    }

    const GWL_EXSTYLE: i32 = -20;
    const WS_EX_NOACTIVATE: i32 = 0x0800_0000;

    #[link(name = "dwmapi", kind = "raw-dylib")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            dw_attribute: u32,
            pv_attribute: *const c_void,
            cb_attribute: u32,
        ) -> i32;
    }

    const SW_SHOWNOACTIVATE: i32 = 4;
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWCP_ROUND: u32 = 2;
    // COLORREF is 0x00BBGGRR: subtle 1px border matching light (#dadce0) and dark (#434447) surfaces.
    const LIGHT_BORDER_COLORREF: u32 = 0x00E0_DCDA;
    const DARK_BORDER_COLORREF: u32 = 0x0047_4443;

    pub fn configure_popup_window_builder(
        builder: WindowBuilder,
        main_window: &Window,
    ) -> WindowBuilder {
        builder
            .with_skip_taskbar(true)
            .with_owner_window(main_window.hwnd() as _)
            .with_undecorated_shadow(true)
    }

    pub fn apply_popup_dwm_attributes(win: &Window, is_dark: bool) {
        let hwnd = win.hwnd();
        let dark_flag: i32 = i32::from(is_dark);
        let corner_pref: u32 = DWMWCP_ROUND;
        let border_color: u32 = if is_dark {
            DARK_BORDER_COLORREF
        } else {
            LIGHT_BORDER_COLORREF
        };

        // SAFETY: `hwnd` is a valid window handle owned by `win`, and each attribute pointer
        // matches the exact 4-byte integer size expected by `DwmSetWindowAttribute` on Windows 11.
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
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                ptr::addr_of!(border_color).cast::<c_void>(),
                size_of::<u32>() as u32,
            );
        }
    }

    pub fn set_popup_no_activate(win: &Window, no_activate: bool) {
        let hwnd = win.hwnd();
        // SAFETY: `hwnd` is a valid top-level window handle owned by `win`.
        unsafe {
            let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE);
            let next_style = if no_activate {
                ex_style | WS_EX_NOACTIVATE
            } else {
                ex_style & !WS_EX_NOACTIVATE
            };
            if next_style != ex_style {
                let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, next_style);
            }
        }
    }

    pub fn show_window_no_activate(win: &Window) {
        // SAFETY: `win.hwnd()` is a valid HWND owned by `tao::Window`.
        unsafe {
            ShowWindow(win.hwnd(), SW_SHOWNOACTIVATE);
        }
    }

    pub fn is_foreground_either(main_window: &Window, popup_window: Option<&Window>) -> bool {
        // SAFETY: `GetForegroundWindow` takes no pointers and returns the active HWND or 0.
        let fg = unsafe { GetForegroundWindow() };
        if fg == 0 {
            return false;
        }
        if fg == main_window.hwnd() {
            return true;
        }
        if let Some(pw) = popup_window {
            if fg == pw.hwnd() {
                return true;
            }
        }
        false
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use tao::window::{Window, WindowBuilder};

    pub fn configure_popup_window_builder(
        builder: WindowBuilder,
        _main_window: &Window,
    ) -> WindowBuilder {
        builder.with_always_on_top(true)
    }

    pub fn apply_popup_dwm_attributes(_win: &Window, _is_dark: bool) {}

    pub fn set_popup_no_activate(_win: &Window, _no_activate: bool) {}

    pub fn show_window_no_activate(win: &Window) {
        win.set_visible(true);
    }

    pub fn is_foreground_either(main_window: &Window, popup_window: Option<&Window>) -> bool {
        main_window.is_focused() || popup_window.is_some_and(Window::is_focused)
    }
}

pub const POPUP_HTML: &str = r##"<!DOCTYPE html>
<html lang="en" data-theme="light">
<head>
<meta charset="UTF-8">
<style>
  :root {
    --popup-anim-duration: 120ms;
    --popup-anim-ease: cubic-bezier(0.2, 0, 0, 1);
    --menu-bg: #ffffff;
    --menu-border: rgba(60, 64, 67, 0.15);
    --menu-shadow: 0 4px 16px rgba(0, 0, 0, 0.16), 0 1px 4px rgba(0, 0, 0, 0.08);
    --text-primary: #202124;
    --text-secondary: #5f6368;
    --btn-hover: rgba(60, 64, 67, 0.08);
    --btn-active: rgba(60, 64, 67, 0.14);
    --divider: rgba(60, 64, 67, 0.14);
    --accent: #0067c0;
  }

  html[data-theme="dark"] {
    --menu-bg: #292a2d;
    --menu-border: rgba(255, 255, 255, 0.12);
    --menu-shadow: 0 6px 20px rgba(0, 0, 0, 0.45), 0 1px 4px rgba(0, 0, 0, 0.3);
    --text-primary: #e8eaed;
    --text-secondary: #9aa0a6;
    --btn-hover: rgba(232, 234, 237, 0.09);
    --btn-active: rgba(232, 234, 237, 0.16);
    --divider: rgba(232, 234, 237, 0.12);
    --accent: #60cdff;
  }

  @media (prefers-reduced-motion: reduce) {
    :root { --popup-anim-duration: 0ms !important; }
  }
  html[data-reduced-motion="true"] {
    --popup-anim-duration: 0ms !important;
  }

  * {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
    user-select: none;
    -webkit-user-select: none;
  }

  html, body {
    width: 100%;
    height: 100%;
    background: var(--menu-bg);
    overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    font-size: 13px;
    color: var(--text-primary);
  }

  .popup-stage {
    padding: 0;
    width: 100%;
    height: 100%;
    background: var(--menu-bg);
  }

  .popup-card {
    width: 100%;
    height: 100%;
    background: var(--menu-bg);
    padding: 6px 0;
    opacity: 1;
    transform: translateY(-4px);
    transition:
      opacity var(--popup-anim-duration) var(--popup-anim-ease),
      transform var(--popup-anim-duration) var(--popup-anim-ease);
  }

  .popup-card.open {
    opacity: 1;
    transform: translateY(0);
  }

  .menu-item {
    width: 100%;
    height: 32px;
    padding: 0 14px;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-family: inherit;
    font-size: 13px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    cursor: pointer;
    text-align: left;
    outline: none;
    transition: background-color 80ms ease;
  }

  .menu-item:hover,
  .menu-item.focused {
    background: var(--btn-hover);
  }

  .menu-item:active {
    background: var(--btn-active);
  }

  .menu-item-label {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .menu-item-icon {
    width: 16px;
    height: 16px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--text-secondary);
    flex-shrink: 0;
  }

  .menu-item-icon svg {
    width: 15px;
    height: 15px;
    fill: currentColor;
  }

  .menu-shortcut {
    font-size: 11.5px;
    color: var(--text-secondary);
    margin-left: 16px;
  }

  .menu-sep {
    height: 1px;
    background: var(--divider);
    margin: 5px 0;
  }

  .menu-zoom-row {
    height: 34px;
    padding: 0 14px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 13px;
  }

  .zoom-controls {
    display: inline-flex;
    align-items: center;
    border: 1px solid var(--divider);
    border-radius: 14px;
    overflow: hidden;
  }

  .zoom-btn {
    width: 28px;
    height: 24px;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-size: 14px;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: background-color 80ms ease;
  }

  .zoom-btn:hover {
    background: var(--btn-hover);
  }

  .zoom-btn:active {
    background: var(--btn-active);
  }

  .zoom-val {
    min-width: 44px;
    height: 24px;
    border: none;
    border-left: 1px solid var(--divider);
    border-right: 1px solid var(--divider);
    background: transparent;
    color: var(--text-primary);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    font-variant-numeric: tabular-nums;
  }

  .zoom-val:hover {
    background: var(--btn-hover);
  }

  .omnibox-item {
    width: 100%;
    height: 38px;
    padding: 0 14px;
    border: none;
    background: transparent;
    color: var(--text-primary);
    font-family: inherit;
    font-size: 13px;
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
    text-align: left;
    outline: none;
    transition: background-color 80ms ease;
  }

  .omnibox-item:hover,
  .omnibox-item.focused {
    background: var(--btn-hover);
  }

  .omnibox-item.default-match {
    background: rgba(128, 128, 128, 0.12);
    box-shadow: inset 2px 0 0 var(--accent-color, #4c8bf5);
  }

  .omnibox-item:active {
    background: var(--btn-active);
  }

  .omnibox-icon {
    width: 16px;
    height: 16px;
    color: var(--text-secondary);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .omnibox-icon svg {
    width: 15px;
    height: 15px;
    fill: currentColor;
  }

  .omnibox-text {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
    flex: 1;
    overflow: hidden;
  }

  .omnibox-title {
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex-shrink: 1;
  }

  .omnibox-url {
    color: var(--text-secondary);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex-shrink: 2;
  }
</style>
</head>
<body>
  <div class="popup-stage">
    <div id="popup-card" class="popup-card" role="menu"></div>
  </div>
  <script>
    (function () {
      const card = document.getElementById("popup-card");
      let currentKind = null;
      let closeTimer = null;
      let focusedIdx = -1;

      function postIpc(msg) {
        if (window.ipc && typeof window.ipc.postMessage === "function") {
          window.ipc.postMessage(msg);
        }
      }

      function renderMenu(data) {
        const zoom = (data && data.zoomPercent) ? data.zoomPercent : 100;
        card.innerHTML = [
          '<button class="menu-item" data-cmd="new_tab" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/></svg></span>New tab</span>',
            '<span class="menu-shortcut">Ctrl+T</span>',
          '</button>',
          '<button class="menu-item" data-cmd="new_window" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M19 4H5a2 2 0 00-2 2v12a2 2 0 002 2h14a2 2 0 002-2V6a2 2 0 00-2-2zm0 14H5V8h14v10z"/></svg></span>New window</span>',
            '<span class="menu-shortcut">Ctrl+N</span>',
          '</button>',
          '<div class="menu-sep" role="separator"></div>',
          '<button class="menu-item" data-cmd="open_new_tab:ember://history" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M13 3a9 9 0 00-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0013 21a9 9 0 000-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z"/></svg></span>History</span>',
            '<span class="menu-shortcut">Ctrl+H</span>',
          '</button>',
          '<button class="menu-item" data-cmd="open_new_tab:ember://downloads" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z"/></svg></span>Downloads</span>',
            '<span class="menu-shortcut">Ctrl+J</span>',
          '</button>',
          '<button class="menu-item" data-cmd="open_new_tab:ember://bookmarks" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"/></svg></span>Bookmarks</span>',
            '<span class="menu-shortcut">Ctrl+Shift+O</span>',
          '</button>',
          '<div class="menu-sep" role="separator"></div>',
          '<div class="menu-zoom-row">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0016 9.5 6.5 6.5 0 109.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"/></svg></span>Zoom</span>',
            '<div class="zoom-controls">',
              '<button id="z-out" class="zoom-btn" title="Zoom out (Ctrl+-)" aria-label="Zoom out">&#8722;</button>',
              '<button id="z-reset" class="zoom-val" title="Reset zoom (Ctrl+0)">' + zoom + '%</button>',
              '<button id="z-in" class="zoom-btn" title="Zoom in (Ctrl+=)" aria-label="Zoom in">+</button>',
            '</div>',
          '</div>',
          '<div class="menu-sep" role="separator"></div>',
          '<button class="menu-item" data-cmd="print" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M19 8H5c-1.66 0-3 1.34-3 3v6h4v4h12v-4h4v-6c0-1.66-1.34-3-3-3zm-3 11H8v-5h8v5zm3-7c-.55 0-1-.45-1-1s.45-1 1-1 1 .45 1 1-.45 1-1 1zm-1-9H6v4h12V3z"/></svg></span>Print...</span>',
            '<span class="menu-shortcut">Ctrl+P</span>',
          '</button>',
          '<button class="menu-item" data-cmd="open_find" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0016 9.5 6.5 6.5 0 109.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"/></svg></span>Find in page...</span>',
            '<span class="menu-shortcut">Ctrl+F</span>',
          '</button>',
          '<div class="menu-sep" role="separator"></div>',
          '<button class="menu-item" data-cmd="open_new_tab:ember://settings" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 00.12-.61l-1.92-3.32a.49.49 0 00-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 00-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 00-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.07.62-.07.94s.02.64.07.94l-2.03 1.58a.49.49 0 00-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6A3.6 3.6 0 1112 8.4a3.6 3.6 0 010 7.2z"/></svg></span>Settings</span>',
            '<span class="menu-shortcut"></span>',
          '</button>',
          '<button class="menu-item" data-cmd="window_close" role="menuitem">',
            '<span class="menu-item-label"><span class="menu-item-icon"><svg viewBox="0 0 24 24"><path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/></svg></span>Exit</span>',
            '<span class="menu-shortcut">Alt+F4</span>',
          '</button>'
        ].join("");

        card.querySelectorAll(".menu-item").forEach(function (btn) {
          btn.addEventListener("click", function () {
            const cmd = btn.getAttribute("data-cmd");
            if (cmd) {
              postIpc("popup_action:" + cmd);
            }
          });
        });
        const zOut = document.getElementById("z-out");
        const zReset = document.getElementById("z-reset");
        const zIn = document.getElementById("z-in");
        if (zOut) zOut.addEventListener("click", function () { postIpc("popup_keep_open:zoom_out"); });
        if (zReset) zReset.addEventListener("click", function () { postIpc("popup_keep_open:zoom_reset"); });
        if (zIn) zIn.addEventListener("click", function () { postIpc("popup_keep_open:zoom_in"); });
      }

      function updateFocus(items) {
        items.forEach(function (el, idx) {
          el.classList.toggle("focused", idx === focusedIdx);
        });
      }

      window.addEventListener("keydown", function (e) {
        if (e.key === "Escape") {
          e.preventDefault();
          postIpc("popup_close_request");
          return;
        }
        const items = Array.from(card.querySelectorAll(".menu-item"));
        if (items.length === 0) return;
        if (e.key === "ArrowDown") {
          e.preventDefault();
          focusedIdx = (focusedIdx + 1) % items.length;
          updateFocus(items);
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          focusedIdx = (focusedIdx - 1 + items.length) % items.length;
          updateFocus(items);
        } else if (e.key === "Enter" && focusedIdx >= 0 && items[focusedIdx]) {
          e.preventDefault();
          items[focusedIdx].click();
        }
      });

      function omniboxIconSvg(kind) {
        if (kind === "bookmark") {
          return '<svg viewBox="0 0 24 24"><path d="M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z"/></svg>';
        }
        if (kind === "search") {
          return '<svg viewBox="0 0 24 24"><path d="M15.5 14h-.79l-.28-.27A6.471 6.471 0 0016 9.5 6.5 6.5 0 109.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z"/></svg>';
        }
        return '<svg viewBox="0 0 24 24"><path d="M13 3a9 9 0 00-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0013 21a9 9 0 000-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z"/></svg>';
      }

      function renderOmnibox(data) {
        const items = (data && Array.isArray(data.items)) ? data.items : [];
        const selected = (data && typeof data.selectedIndex === "number") ? data.selectedIndex : -1;
        card.innerHTML = "";
        items.forEach(function (item, idx) {
          const btn = document.createElement("button");
          btn.className = "omnibox-item"
            + (item.isDefault ? " default-match" : "")
            + (idx === selected ? " focused" : "");
          btn.setAttribute("role", "option");
          btn.setAttribute("aria-selected", (idx === selected || Boolean(item.isDefault)) ? "true" : "false");

          const icon = document.createElement("span");
          icon.className = "omnibox-icon";
          icon.innerHTML = omniboxIconSvg(item.kind);

          const textWrap = document.createElement("span");
          textWrap.className = "omnibox-text";

          const titleEl = document.createElement("span");
          titleEl.className = "omnibox-title";
          titleEl.textContent = item.title || item.url || "";
          textWrap.appendChild(titleEl);

          if (item.kind !== "search" && item.url && item.url !== item.title) {
            const urlEl = document.createElement("span");
            urlEl.className = "omnibox-url";
            urlEl.textContent = item.url;
            textWrap.appendChild(urlEl);
          }

          btn.appendChild(icon);
          btn.appendChild(textWrap);
          btn.addEventListener("mousedown", function (e) {
            e.preventDefault();
          });
          btn.addEventListener("click", function () {
            if (item.url) {
              postIpc("popup_action:navigate:" + item.url);
            }
          });
          card.appendChild(btn);
        });
      }

      window.__emberOpenPopup = function (kind, data, isDark, reducedMotion, gen) {
        if (closeTimer) {
          clearTimeout(closeTimer);
          closeTimer = null;
        }
        currentKind = kind;
        focusedIdx = -1;
        document.documentElement.setAttribute("data-theme", isDark ? "dark" : "light");
        document.documentElement.setAttribute("data-reduced-motion", reducedMotion ? "true" : "false");

        if (kind === "menu") {
          card.style.transformOrigin = "top right";
          renderMenu(data);
        } else if (kind === "omnibox") {
          card.style.transformOrigin = "top left";
          renderOmnibox(data);
        } else {
          card.style.transformOrigin = "top left";
          card.innerHTML = "";
        }

        card.classList.remove("open");
        requestAnimationFrame(function () {
          requestAnimationFrame(function () {
            card.classList.add("open");
            postIpc("popup_ready:" + (gen || 0));
          });
        });
      };

      window.__emberUpdatePopupData = function (data) {
        if (currentKind === "menu" && data && typeof data.zoomPercent === "number") {
          const zReset = document.getElementById("z-reset");
          if (zReset) zReset.textContent = data.zoomPercent + "%";
        } else if (currentKind === "omnibox" && data) {
          if (Array.isArray(data.items)) {
            renderOmnibox(data);
          } else if (typeof data.selectedIndex === "number") {
            const rows = card.querySelectorAll(".omnibox-item");
            rows.forEach(function (el, idx) {
              el.classList.toggle("focused", idx === data.selectedIndex);
            });
          }
        }
      };

      window.__emberClosePopup = function (gen) {
        card.classList.remove("open");
        const reduced = document.documentElement.getAttribute("data-reduced-motion") === "true"
          || window.matchMedia("(prefers-reduced-motion: reduce)").matches;
        const delay = reduced ? 0 : 125;
        if (closeTimer) clearTimeout(closeTimer);
        closeTimer = setTimeout(function () {
          closeTimer = null;
          postIpc("popup_closed:" + (gen || 0));
        }, delay);
      };

      postIpc("popup_init_ready");
    })();
  </script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_popup_state_machine_open_ready_close_idempotent() {
        let mut sm = PopupStateMachine::new();
        let t0 = Instant::now();
        assert_eq!(sm.state(), &PopupState::Closed);
        assert!(!sm.is_open());

        let OpenDecision::StartOpening { generation: g1 } =
            sm.request_open(PopupKind::Menu, "menu-btn", true, t0)
        else {
            panic!("expected StartOpening");
        };
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                kind: PopupKind::Menu,
                anchor_id: String::from("menu-btn"),
            }
        );

        // Stale generation is ignored.
        assert_eq!(sm.on_ready_to_show(g1 + 10), None);
        assert_eq!(sm.on_ready_to_show(g1), Some(PopupKind::Menu));
        assert_eq!(
            sm.state(),
            &PopupState::Open {
                kind: PopupKind::Menu,
                anchor_id: String::from("menu-btn"),
            }
        );

        // Closing is idempotent.
        assert!(sm
            .request_close(CloseReason::Explicit, t0 + Duration::from_millis(20))
            .is_some());
        assert_eq!(sm.state(), &PopupState::Closed);
        assert!(sm
            .request_close(CloseReason::Explicit, t0 + Duration::from_millis(30))
            .is_none());
    }

    #[test]
    fn test_popup_state_machine_toggle_suppression_after_blur_race() {
        let mut sm = PopupStateMachine::new();
        let t0 = Instant::now();

        let OpenDecision::StartOpening { generation: g1 } =
            sm.request_open(PopupKind::Menu, "menu-btn", true, t0)
        else {
            panic!("expected StartOpening");
        };
        assert_eq!(sm.on_ready_to_show(g1), Some(PopupKind::Menu));

        // Clicking menu-btn causes window blur -> closes popup.
        let t_blur = t0 + Duration::from_millis(100);
        assert!(sm
            .request_close(CloseReason::BlurOrOutsideClick, t_blur)
            .is_some());
        assert_eq!(sm.state(), &PopupState::Closed);

        // 50ms later, menu-btn's click handler fires -> suppressed!
        let t_click = t_blur + Duration::from_millis(50);
        assert_eq!(
            sm.request_open(PopupKind::Menu, "menu-btn", true, t_click),
            OpenDecision::SuppressedToggle
        );
        assert_eq!(sm.state(), &PopupState::Closed);

        // Clicking again after the suppression window opens normally.
        let t_next = t_click + Duration::from_millis(300);
        assert!(matches!(
            sm.request_open(PopupKind::Menu, "menu-btn", true, t_next),
            OpenDecision::StartOpening { .. }
        ));
    }

    #[test]
    fn test_popup_state_machine_toggle_while_open_and_kind_switch() {
        let mut sm = PopupStateMachine::new();
        let t0 = Instant::now();

        let OpenDecision::StartOpening { generation: g1 } =
            sm.request_open(PopupKind::Omnibox, "omnibox", false, t0)
        else {
            panic!("expected StartOpening");
        };
        assert_eq!(sm.on_ready_to_show(g1), Some(PopupKind::Omnibox));

        // Typing updates in place without bumping generation.
        assert_eq!(
            sm.request_open(
                PopupKind::Omnibox,
                "omnibox",
                false,
                t0 + Duration::from_millis(50)
            ),
            OpenDecision::UpdateInPlace { generation: g1 }
        );

        // Opening Menu while Omnibox is open transitions cleanly to Opening(Menu).
        let OpenDecision::StartOpening { generation: g2 } = sm.request_open(
            PopupKind::Menu,
            "menu-btn",
            true,
            t0 + Duration::from_millis(80),
        ) else {
            panic!("expected StartOpening for Menu");
        };
        assert!(g2 > g1);
        assert_eq!(sm.on_ready_to_show(g2), Some(PopupKind::Menu));

        // Toggling same anchor while Open closes it instead of reopening.
        assert!(matches!(
            sm.request_open(
                PopupKind::Menu,
                "menu-btn",
                true,
                t0 + Duration::from_millis(120)
            ),
            OpenDecision::ClosedInstead { .. }
        ));
        assert_eq!(sm.state(), &PopupState::Closed);
    }

    #[test]
    fn test_popup_regression_20_cycles_all_close_reasons() {
        let mut sm = PopupStateMachine::new();
        let mut t = Instant::now();
        let reasons = [
            CloseReason::BlurOrOutsideClick,
            CloseReason::Explicit,
            CloseReason::SystemEvent,
        ];

        for i in 0..20usize {
            let kind = if i % 2 == 0 {
                PopupKind::Menu
            } else {
                PopupKind::Omnibox
            };
            let anchor = if kind == PopupKind::Menu {
                "menu-btn"
            } else {
                "omnibox"
            };
            let toggle = kind == PopupKind::Menu;
            t += Duration::from_millis(300);

            let OpenDecision::StartOpening { generation } =
                sm.request_open(kind, anchor, toggle, t)
            else {
                panic!("cycle {i}: expected StartOpening");
            };
            assert_eq!(sm.on_ready_to_show(generation), Some(kind));
            assert!(sm.is_open());

            if i % 5 == 4 && toggle {
                assert!(matches!(
                    sm.request_open(kind, anchor, true, t + Duration::from_millis(50)),
                    OpenDecision::ClosedInstead { .. }
                ));
            } else {
                let reason = reasons[i % reasons.len()];
                assert!(sm
                    .request_close(reason, t + Duration::from_millis(50))
                    .is_some());
            }
            assert_eq!(sm.state(), &PopupState::Closed);
            assert!(!sm.is_open());
        }
    }

    #[test]
    fn test_omnibox_sequence_ordering_out_of_order_duplicate_and_cancelled() {
        // Normal increasing sequence
        assert!(should_apply_omnibox_response(1, 1, 0, 0));
        assert!(should_apply_omnibox_response(2, 2, 1, 0));

        // Duplicate response for same query_id is allowed if it matches latest_sent_id
        // (e.g. local cache followed by async online suggestions for the same keystroke)
        assert!(should_apply_omnibox_response(2, 2, 2, 0));

        // Out-of-order stale response (query_id < latest_sent_id or < last_applied_id) is rejected
        assert!(!should_apply_omnibox_response(1, 3, 2, 0));
        assert!(!should_apply_omnibox_response(2, 3, 2, 0));

        // Cancelled response (e.g. Esc/blur/Enter closed popup at seq 3) is rejected
        assert!(!should_apply_omnibox_response(3, 3, 2, 3));
        // Only a newer user keystroke (seq 4) can reopen
        assert!(should_apply_omnibox_response(4, 4, 2, 3));
    }

    #[test]
    fn test_popup_30_cycles_type_delete_retype_click_away_enter_esc() {
        let mut sm = PopupStateMachine::new();
        let mut t = Instant::now();

        for i in 0..30usize {
            t += Duration::from_millis(260);
            // Type -> Opening -> UpdateWhileOpening -> Open -> UpdateInPlace (delete/retype)
            let OpenDecision::StartOpening { generation: g } =
                sm.request_open(PopupKind::Omnibox, "omnibox", false, t)
            else {
                panic!("cycle {i}: expected StartOpening");
            };
            assert!(matches!(
                sm.request_open(
                    PopupKind::Omnibox,
                    "omnibox",
                    false,
                    t + Duration::from_millis(10)
                ),
                OpenDecision::UpdateWhileOpening { .. }
            ));
            assert_eq!(sm.on_ready_to_show(g), Some(PopupKind::Omnibox));
            assert!(matches!(
                sm.request_open(
                    PopupKind::Omnibox,
                    "omnibox",
                    false,
                    t + Duration::from_millis(60)
                ),
                OpenDecision::UpdateInPlace { .. }
            ));

            // Alternate close triggers: click away (BlurOrOutsideClick), suggestion click / Enter / Esc (Explicit), SystemEvent
            let reason = match i % 3 {
                0 => CloseReason::BlurOrOutsideClick,
                1 => CloseReason::Explicit,
                _ => CloseReason::SystemEvent,
            };
            assert!(sm
                .request_close(reason, t + Duration::from_millis(120))
                .is_some());
            // Idempotent second close does nothing
            assert!(sm
                .request_close(reason, t + Duration::from_millis(125))
                .is_none());
            assert_eq!(sm.state(), &PopupState::Closed);
        }
    }
}
