use std::time::{Duration, Instant};

use wry::{Rect, WebView};

/// Maximum number of recently closed tabs retained for `Ctrl/Cmd+Shift+T`.
pub const MAX_RECENTLY_CLOSED: usize = 10;

/// Default inactivity timeout before background tabs are suspended (5 minutes).
pub const DEFAULT_SUSPEND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Lightweight snapshot of a tab's state for syncing with the Chrome UI.
#[derive(Debug, Clone, PartialEq)]
pub struct TabSummary {
    pub id: usize,
    pub title: String,
    pub url: String,
    pub favicon_url: Option<String>,
    pub is_loading: bool,
    pub is_suspended: bool,
    pub blocked_count: usize,
}

/// Snapshot of a closed tab stored in the recently-closed stack for `Ctrl/Cmd+Shift+T`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedTab {
    pub title: String,
    pub url: String,
    pub favicon_url: Option<String>,
    pub scroll_position: (f64, f64),
}

/// Represents a single browser tab and its lazily-created / suspendable page WebView.
pub struct Tab {
    pub id: usize,
    pub title: String,
    pub url: String,
    pub favicon_url: Option<String>,
    pub is_loading: bool,
    pub is_suspended: bool,
    pub blocked_count: usize,
    pub scroll_position: (f64, f64),
    pub pending_scroll_restore: bool,
    pub last_active_at: Instant,
    pub webview: Option<WebView>,
    /// URL the user typed into the omnibox for the navigation currently in flight.
    /// When the load finishes, the tab's *final* URL (after redirects) is marked as typed.
    pub pending_typed_url: Option<String>,
    /// `true` while the tab shows the inline "blocked by the ad blocker" notice, so the
    /// synthetic `about:blank` load does not overwrite the blocked URL shown in the omnibox.
    pub showing_blocked_notice: bool,
    /// Last trusted user activation (pointer/key input) reported by this tab's page.
    pub last_user_activation: Option<Instant>,
    /// Last time this tab was allowed to open a popup / new tab.
    pub last_popup_at: Option<Instant>,
}

/// Outcome of closing a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTabOutcome {
    /// Tab was closed and another tab remains active.
    Closed,
    /// The last remaining tab was closed, so the browser window should close.
    LastTabClosed,
    /// No tab with the given ID exists; nothing changed.
    NotFound,
}

/// How long after a user activation (click / key press) a page may open a new tab.
pub const POPUP_ACTIVATION_WINDOW: Duration = Duration::from_secs(1);

/// Popup-blocker decision: a page may open a new tab only if it saw a user activation within
/// [`POPUP_ACTIVATION_WINDOW`] and that activation has not already been used by an earlier popup.
pub fn popup_allowed(
    last_activation: Option<Instant>,
    last_popup: Option<Instant>,
    now: Instant,
) -> bool {
    let Some(activation) = last_activation else {
        return false;
    };
    if activation > now || now.duration_since(activation) > POPUP_ACTIVATION_WINDOW {
        return false;
    }
    // One popup per activation.
    !last_popup.is_some_and(|popup| popup >= activation)
}

/// Strips the `#fragment` part of a URL.
pub fn strip_url_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

/// Decides whether an SPA navigation (`pushState` / `replaceState` / `hashchange`) from
/// `old_url` to `new_url` should create a history visit: only when the URL actually changed,
/// ignoring the fragment.
pub fn should_record_spa_visit(old_url: &str, new_url: &str) -> bool {
    strip_url_fragment(old_url.trim()) != strip_url_fragment(new_url.trim())
}

/// Manages open tabs, lazy WebView instantiation, background suspension, and recently closed tabs.
pub struct TabManager {
    tabs: Vec<Tab>,
    active_tab_id: usize,
    next_id: usize,
    recently_closed: Vec<ClosedTab>,
    suspend_timeout: Duration,
    /// Closing tab WebViews kept visible until the replacement tab finishes its first paint.
    retiring_webviews: Vec<WebView>,
}

impl Default for TabManager {
    fn default() -> Self {
        Self::new(DEFAULT_SUSPEND_TIMEOUT)
    }
}

impl TabManager {
    /// Creates an empty tab manager with the specified background tab suspension timeout.
    pub fn new(suspend_timeout: Duration) -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_id: 0,
            next_id: 1,
            recently_closed: Vec::new(),
            suspend_timeout,
            retiring_webviews: Vec::new(),
        }
    }

    /// Returns the configured background tab suspension timeout.
    pub fn suspend_timeout(&self) -> Duration {
        self.suspend_timeout
    }

    /// Changes the background tab suspension timeout (applies from the next suspension check).
    pub fn set_suspend_timeout(&mut self, timeout: Duration) {
        self.suspend_timeout = timeout;
    }

    /// Calls `f` for every tab that currently has a live `WebView`.
    pub fn for_each_webview(&self, mut f: impl FnMut(&WebView)) {
        for tab in &self.tabs {
            if let Some(webview) = &tab.webview {
                f(webview);
            }
        }
    }

    /// Creates a new tab entry lazily (`webview: None`).
    /// Its WebView is only created when the tab is active and materialized by the application shell.
    pub fn create_tab(&mut self, initial_url: &str, activate: bool) -> usize {
        let id = self.allocate_id();
        let now = Instant::now();
        let default_favicon = derive_default_favicon(initial_url);
        let initial_title = match initial_url.trim() {
            "ember://newtab" => String::from("New Tab"),
            "ember://history" => String::from("History"),
            "ember://bookmarks" => String::from("Bookmarks"),
            "ember://downloads" => String::from("Downloads"),
            "ember://settings" => String::from("Settings"),
            other => other.to_owned(),
        };

        let tab = Tab {
            id,
            title: initial_title,
            url: initial_url.to_owned(),
            favicon_url: default_favicon,
            is_loading: false,
            is_suspended: false,
            blocked_count: 0,
            scroll_position: (0.0, 0.0),
            pending_scroll_restore: false,
            last_active_at: now,
            webview: None,
            pending_typed_url: None,
            showing_blocked_notice: false,
            last_user_activation: None,
            last_popup_at: None,
        };

        self.tabs.push(tab);

        if activate || self.active_tab_id == 0 {
            self.switch_to(id);
        }

        id
    }

    /// Opens an internal page (`ember://settings`, `ember://history`, `ember://bookmarks`,
    /// `ember://downloads`) in a new foreground tab, or switches to an already-open tab
    /// displaying that canonical URL instead of opening a duplicate.
    /// Returns `(tab_id, created_new)`.
    pub fn open_or_focus_internal_page(&mut self, url: &str) -> (usize, bool) {
        let canonical = crate::pages::canonical_internal_url(url).unwrap_or(url.trim());
        if let Some(existing_id) = self
            .tabs
            .iter()
            .find(|t| crate::pages::canonical_internal_url(&t.url) == Some(canonical))
            .map(|t| t.id)
        {
            self.switch_to(existing_id);
            (existing_id, false)
        } else {
            let new_id = self.create_tab(canonical, true);
            (new_id, true)
        }
    }

    /// Duplicates an existing tab right next to the source tab, preserving URL, title, favicon, and scroll position.
    pub fn duplicate_tab(&mut self, source_tab_id: usize, activate: bool) -> Option<usize> {
        let source_index = self.tabs.iter().position(|t| t.id == source_tab_id)?;
        let source = &self.tabs[source_index];

        let title = source.title.clone();
        let url = source.url.clone();
        let favicon_url = source.favicon_url.clone();
        let scroll_position = source.scroll_position;

        let new_id = self.allocate_id();
        let new_tab = Tab {
            id: new_id,
            title,
            url,
            favicon_url,
            is_loading: false,
            is_suspended: false,
            blocked_count: 0,
            scroll_position,
            pending_scroll_restore: scroll_position != (0.0, 0.0),
            last_active_at: Instant::now(),
            webview: None,
            pending_typed_url: None,
            showing_blocked_notice: false,
            last_user_activation: None,
            last_popup_at: None,
        };

        self.tabs.insert(source_index + 1, new_tab);

        if activate {
            self.switch_to(new_id);
        }

        Some(new_id)
    }

    /// Reorders tabs by moving `dragged_tab_id` to the position of `target_tab_id`.
    pub fn reorder_tab(&mut self, dragged_tab_id: usize, target_tab_id: usize) -> bool {
        if dragged_tab_id == target_tab_id {
            return false;
        }
        let Some(from_idx) = self.tabs.iter().position(|t| t.id == dragged_tab_id) else {
            return false;
        };
        let Some(to_idx) = self.tabs.iter().position(|t| t.id == target_tab_id) else {
            return false;
        };
        self.reorder_by_index(from_idx, to_idx)
    }

    /// Reorders a tab from `from_index` to `to_index`.
    pub fn reorder_by_index(&mut self, from_index: usize, to_index: usize) -> bool {
        if from_index >= self.tabs.len() || to_index >= self.tabs.len() || from_index == to_index {
            return false;
        }
        let tab = self.tabs.remove(from_index);
        self.tabs.insert(to_index, tab);
        true
    }

    /// Switches the active tab to `tab_id`, updating activity timestamps and WebView visibility.
    /// If the target tab was suspended or lazily created, its `is_suspended` flag is cleared so the
    /// shell can materialize its WebView on demand.
    pub fn switch_to(&mut self, tab_id: usize) -> bool {
        if !self.tabs.iter().any(|t| t.id == tab_id) {
            return false;
        }

        let now = Instant::now();
        let prev_active_id = self.active_tab_id;
        self.active_tab_id = tab_id;

        let target_has_webview = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .is_some_and(|t| t.webview.is_some());

        for tab in &mut self.tabs {
            if tab.id == prev_active_id || tab.id == tab_id {
                tab.last_active_at = now;
            }
            if tab.id == tab_id && tab.is_suspended {
                tab.is_suspended = false;
                tab.pending_scroll_restore = true;
            }
        }
        // If the target tab already has a materialized WebView, show it FIRST before hiding
        // any other WebView so an empty window background is never exposed between swaps.
        if target_has_webview {
            if let Some(target_wv) = self
                .tabs
                .iter()
                .find(|t| t.id == tab_id)
                .and_then(|t| t.webview.as_ref())
            {
                let _ = target_wv.set_visible(true);
                let _ = target_wv.focus();
            }
            for tab in &self.tabs {
                if tab.id != tab_id {
                    if let Some(webview) = &tab.webview {
                        let _ = webview.set_visible(false);
                    }
                }
            }
            self.retire_all_old_webviews();
        }
        true
    }

    /// Switches to the next tab in the tab strip (`Ctrl+Tab`), wrapping around to the first tab.
    pub fn switch_next_tab(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        let current_idx = self
            .tabs
            .iter()
            .position(|t| t.id == self.active_tab_id)
            .unwrap_or(0);
        let next_idx = (current_idx + 1) % self.tabs.len();
        let next_id = self.tabs[next_idx].id;
        self.switch_to(next_id)
    }

    /// Switches to the previous tab in the tab strip (`Ctrl+Shift+Tab`), wrapping around to the last tab.
    pub fn switch_prev_tab(&mut self) -> bool {
        if self.tabs.len() <= 1 {
            return false;
        }
        let current_idx = self
            .tabs
            .iter()
            .position(|t| t.id == self.active_tab_id)
            .unwrap_or(0);
        let prev_idx = if current_idx == 0 {
            self.tabs.len() - 1
        } else {
            current_idx - 1
        };
        let prev_id = self.tabs[prev_idx].id;
        self.switch_to(prev_id)
    }

    /// Switches to a tab by 1-based shortcut number (`Ctrl/Cmd+1..9`, where `9` always selects the last tab).
    pub fn switch_to_shortcut_number(&mut self, number: usize) -> bool {
        if self.tabs.is_empty() || !(1..=9).contains(&number) {
            return false;
        }
        let target_idx = if number == 9 {
            self.tabs.len() - 1
        } else if number <= self.tabs.len() {
            number - 1
        } else {
            return false;
        };
        let target_id = self.tabs[target_idx].id;
        self.switch_to(target_id)
    }

    /// Closes the specified tab, records it in the recently-closed stack, and activates an adjacent tab.
    pub fn close_tab(&mut self, tab_id: usize) -> CloseTabOutcome {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return CloseTabOutcome::NotFound;
        };

        let mut removed = self.tabs.remove(index);
        let removed_wv = removed.webview.take();

        let closed_entry = ClosedTab {
            title: removed.title,
            url: removed.url,
            favicon_url: removed.favicon_url,
            scroll_position: removed.scroll_position,
        };

        if self.recently_closed.len() >= MAX_RECENTLY_CLOSED {
            self.recently_closed.remove(0);
        }
        self.recently_closed.push(closed_entry);

        if self.tabs.is_empty() {
            if let Some(wv) = removed_wv {
                let _ = wv.set_visible(false);
            }
            self.retire_all_old_webviews();
            self.active_tab_id = 0;
            return CloseTabOutcome::LastTabClosed;
        }

        if self.active_tab_id == tab_id {
            let next_index = index.min(self.tabs.len() - 1);
            let next_id = self.tabs[next_index].id;
            self.switch_to(next_id);
            let next_is_live = self
                .tabs
                .iter()
                .find(|t| t.id == next_id)
                .is_some_and(|t| t.webview.is_some());
            if let Some(wv) = removed_wv {
                if next_is_live {
                    // Replacement tab is already visible on top; safe to hide and drop now.
                    let _ = wv.set_visible(false);
                } else {
                    // Keep the closing WebView visible until `next_id` finishes its first paint.
                    self.retiring_webviews.push(wv);
                }
            }
        } else if let Some(wv) = removed_wv {
            let _ = wv.set_visible(false);
        }

        CloseTabOutcome::Closed
    }

    /// Reopens the most recently closed tab (`Ctrl/Cmd+Shift+T`), restoring its URL, title, favicon,
    /// and scroll position, and makes it the active tab.
    pub fn reopen_closed_tab(&mut self) -> Option<usize> {
        let closed = self.recently_closed.pop()?;
        let new_id = self.allocate_id();

        let tab = Tab {
            id: new_id,
            title: closed.title,
            url: closed.url,
            favicon_url: closed.favicon_url,
            is_loading: false,
            is_suspended: false,
            blocked_count: 0,
            scroll_position: closed.scroll_position,
            pending_scroll_restore: closed.scroll_position != (0.0, 0.0),
            last_active_at: Instant::now(),
            webview: None,
            pending_typed_url: None,
            showing_blocked_notice: false,
            last_user_activation: None,
            last_popup_at: None,
        };

        self.tabs.push(tab);
        self.switch_to(new_id);
        Some(new_id)
    }

    /// Returns the stack of recently closed tabs (oldest to newest).
    pub fn recently_closed(&self) -> &[ClosedTab] {
        &self.recently_closed
    }

    /// Returns `Some((tab_id, url))` if the currently active tab does not yet have a live `WebView`
    /// (either because it was lazily created or because it was restored from suspension).
    pub fn active_tab_needs_webview(&self) -> Option<(usize, String)> {
        let active = self.active_tab()?;
        if active.webview.is_none() {
            Some((active.id, active.url.clone()))
        } else {
            None
        }
    }

    /// Attaches a newly created `WebView` to `tab_id`, keeping it hidden until `reveal_tab`
    /// is called after its first paint (or fallback timeout).
    pub fn attach_webview(&mut self, tab_id: usize, webview: WebView) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            let _ = webview.set_visible(false);
            tab.is_suspended = false;
            tab.is_loading = true;
            tab.last_active_at = Instant::now();
            tab.webview = Some(webview);
            return true;
        }
        false
    }

    /// Reveals `tab_id`'s WebView and hides all other tabs' WebViews when `tab_id` is still active.
    /// When `take_focus` is `false` (e.g. the user is typing in the omnibox) the page is shown
    /// without stealing keyboard focus.
    pub fn reveal_tab(&mut self, tab_id: usize, take_focus: bool) -> bool {
        if self.active_tab_id != tab_id {
            return false;
        }
        let Some(target_wv) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .and_then(|t| t.webview.as_ref())
        else {
            return false;
        };
        // Show the newly painted tab FIRST, then hide old/retiring WebViews so zero frames
        // ever expose the parent window background.
        let _ = target_wv.set_visible(true);
        if take_focus {
            let _ = target_wv.focus();
        }
        for tab in &self.tabs {
            if tab.id != tab_id {
                if let Some(webview) = &tab.webview {
                    let _ = webview.set_visible(false);
                }
            }
        }
        self.retire_all_old_webviews();
        true
    }

    fn retire_all_old_webviews(&mut self) {
        for wv in self.retiring_webviews.drain(..) {
            let _ = wv.set_visible(false);
        }
    }

    /// Explicitly suspends a background tab (`tab_id != active_tab_id`), destroying its `WebView`
    /// while preserving its URL, title, favicon, and scroll position.
    pub fn suspend_tab(&mut self, tab_id: usize) -> bool {
        if tab_id == self.active_tab_id {
            return false;
        }
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            if tab.is_suspended {
                return false;
            }
            if let Some(webview) = tab.webview.take() {
                let _ = webview.set_visible(false);
                drop(webview);
            }
            tab.is_suspended = true;
            tab.is_loading = false;
            tab.pending_scroll_restore = true;
            return true;
        }
        false
    }

    /// Suspends all background tabs that have been inactive for at least `self.suspend_timeout`
    /// relative to `now`. Returns the IDs of all newly suspended tabs.
    pub fn suspend_inactive_tabs(&mut self, now: Instant) -> Vec<usize> {
        let mut suspended_ids = Vec::new();
        let timeout = self.suspend_timeout;
        let active_id = self.active_tab_id;

        for tab in &mut self.tabs {
            if tab.id == active_id || tab.is_suspended {
                continue;
            }
            if now.saturating_duration_since(tab.last_active_at) >= timeout {
                if let Some(webview) = tab.webview.take() {
                    let _ = webview.set_visible(false);
                    drop(webview);
                }
                tab.is_suspended = true;
                tab.is_loading = false;
                tab.pending_scroll_restore = true;
                suspended_ids.push(tab.id);
            }
        }

        suspended_ids
    }

    /// Updates a tab's title when the document title changes.
    pub fn update_title(&mut self, tab_id: usize, title: String) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            let trimmed = title.trim();
            if !trimmed.is_empty() && tab.title != trimmed {
                tab.title = trimmed.to_owned();
                return true;
            }
        }
        false
    }

    /// Updates a tab's favicon URL.
    pub fn update_favicon(&mut self, tab_id: usize, favicon_url: Option<String>) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            let cleaned = favicon_url
                .map(|u| u.trim().to_owned())
                .filter(|u| !u.is_empty());
            if tab.favicon_url != cleaned {
                tab.favicon_url = cleaned;
                return true;
            }
        }
        false
    }

    /// Updates a tab's saved `(scroll_x, scroll_y)` coordinates.
    pub fn update_scroll_position(&mut self, tab_id: usize, scroll_x: f64, scroll_y: f64) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.scroll_position = (scroll_x, scroll_y);
            return true;
        }
        false
    }

    /// Consumes and returns the saved scroll position if the tab has a pending scroll restore after load.
    pub fn take_pending_scroll_restore(&mut self, tab_id: usize) -> Option<(f64, f64)> {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            if tab.pending_scroll_restore {
                tab.pending_scroll_restore = false;
                return Some(tab.scroll_position);
            }
        }
        None
    }

    /// Increments the count of blocked ad/tracker requests for `tab_id`.
    pub fn increment_blocked_count(&mut self, tab_id: usize) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.blocked_count = tab.blocked_count.saturating_add(1);
            return true;
        }
        false
    }

    /// Resets the blocked request counter for `tab_id`.
    pub fn reset_blocked_count(&mut self, tab_id: usize) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.blocked_count = 0;
        }
    }

    /// Updates a tab's current URL and loading state.
    pub fn update_page_load(&mut self, tab_id: usize, url: String, is_loading: bool) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            let is_synthetic_internal_load = (tab.url.starts_with("ember://")
                || tab.showing_blocked_notice)
                && (url == "about:blank" || url.starts_with("data:"));
            if !url.is_empty() && !is_synthetic_internal_load {
                tab.showing_blocked_notice = false;
                if tab.url != url {
                    if !tab.pending_scroll_restore {
                        tab.scroll_position = (0.0, 0.0);
                    }
                    tab.favicon_url = derive_default_favicon(&url);
                }
                if tab.title.is_empty() || tab.title == tab.url {
                    tab.title.clone_from(&url);
                }
                tab.url = url;
            }
            tab.is_loading = is_loading;
            return true;
        }
        false
    }

    /// Remembers (or clears) the omnibox-typed URL for the navigation now starting in `tab_id`.
    pub fn set_pending_typed_url(&mut self, tab_id: usize, typed: Option<String>) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.pending_typed_url = typed;
            return true;
        }
        false
    }

    /// Called when `tab_id` finishes loading: if the load was started from a typed omnibox URL,
    /// consumes the pending marker (only once) and returns the tab's *final* URL to mark as typed.
    pub fn take_typed_url_on_load_finished(&mut self, tab_id: usize) -> Option<String> {
        let tab = self.tabs.iter_mut().find(|t| t.id == tab_id)?;
        if tab.is_loading {
            return None;
        }
        tab.pending_typed_url.take()?;
        let url = tab.url.trim();
        if url.is_empty() || url.starts_with("ember://") || tab.showing_blocked_notice {
            return None;
        }
        Some(url.to_owned())
    }

    /// Records a user activation (pointer down / key press) reported by `tab_id`'s page.
    pub fn note_user_activation(&mut self, tab_id: usize, now: Instant) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
            tab.last_user_activation = Some(now);
            return true;
        }
        false
    }

    /// Popup blocker: returns `true` (and consumes the activation) if `tab_id` may open a new tab now.
    pub fn try_consume_popup(&mut self, tab_id: usize, now: Instant) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return false;
        };
        if popup_allowed(tab.last_user_activation, tab.last_popup_at, now) {
            tab.last_popup_at = Some(now);
            true
        } else {
            false
        }
    }

    /// Returns the ID of the currently active tab.
    pub fn active_tab_id(&self) -> usize {
        self.active_tab_id
    }

    /// Returns a reference to the currently active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active_tab_id)
    }

    /// Returns a mutable reference to the currently active tab.
    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let active_id = self.active_tab_id;
        self.tabs.iter_mut().find(|t| t.id == active_id)
    }

    /// Returns a reference to a tab by ID.
    pub fn get_tab(&self, tab_id: usize) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == tab_id)
    }

    /// Returns a mutable reference to a tab by ID.
    pub fn get_tab_mut(&mut self, tab_id: usize) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == tab_id)
    }

    /// Updates the bounds of all live tab WebViews on window resize.
    pub fn resize_all(&self, content_bounds: Rect) {
        for tab in &self.tabs {
            if let Some(webview) = &tab.webview {
                let _ = webview.set_bounds(content_bounds);
            }
        }
        for wv in &self.retiring_webviews {
            let _ = wv.set_bounds(content_bounds);
        }
    }

    /// Returns lightweight summaries of all open tabs in tab-strip order.
    pub fn summaries(&self) -> Vec<TabSummary> {
        self.tabs
            .iter()
            .map(|tab| TabSummary {
                id: tab.id,
                title: tab.title.clone(),
                url: tab.url.clone(),
                favicon_url: tab.favicon_url.clone(),
                is_loading: tab.is_loading,
                is_suspended: tab.is_suspended,
                blocked_count: tab.blocked_count,
            })
            .collect()
    }

    fn allocate_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Derives a default `<origin>/favicon.ico` URL for `http://` and `https://` URLs,
/// or an inline SVG icon for internal `ember://` pages (`settings`, `history`, `bookmarks`, `downloads`).
pub fn derive_default_favicon(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if let Some(canonical) = crate::pages::canonical_internal_url(trimmed) {
        return match canonical {
            crate::pages::SETTINGS_URL => Some(String::from(
                "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%235f6368'%3E%3Cpath d='M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 00.12-.61l-1.92-3.32a.49.49 0 00-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 00-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 00-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.07.62-.07.94s.02.64.07.94l-2.03 1.58a.49.49 0 00-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6A3.6 3.6 0 1112 8.4a3.6 3.6 0 010 7.2z'/%3E%3C/svg%3E",
            )),
            crate::pages::HISTORY_URL => Some(String::from(
                "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%235f6368'%3E%3Cpath d='M13 3a9 9 0 00-9 9H1l3.89 3.89.07.14L9 12H6c0-3.87 3.13-7 7-7s7 3.13 7 7-3.13 7-7 7c-1.93 0-3.68-.79-4.94-2.06l-1.42 1.42A8.954 8.954 0 0013 21a9 9 0 000-18zm-1 5v5l4.28 2.54.72-1.21-3.5-2.08V8H12z'/%3E%3C/svg%3E",
            )),
            crate::pages::BOOKMARKS_URL => Some(String::from(
                "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%235f6368'%3E%3Cpath d='M12 17.27L18.18 21l-1.64-7.03L22 9.24l-7.19-.61L12 2 9.19 8.63 2 9.24l5.46 4.73L5.82 21z'/%3E%3C/svg%3E",
            )),
            crate::pages::DOWNLOADS_URL => Some(String::from(
                "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='%235f6368'%3E%3Cpath d='M5 20h14v-2H5v2zM19 9h-4V3H9v6H5l7 7 7-7z'/%3E%3C/svg%3E",
            )),
            _ => None,
        };
    }

    let rest = if let Some(r) = trimmed.strip_prefix("https://") {
        Some(("https://", r))
    } else {
        trimmed.strip_prefix("http://").map(|r| ("http://", r))
    };

    let (scheme, after_scheme) = rest?;
    let host_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    if host.is_empty() {
        return None;
    }

    Some(format!("{scheme}{host}/favicon.ico"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lazy_tab_creation_and_switching() {
        let mut manager = TabManager::new(Duration::from_secs(60));

        let tab1 = manager.create_tab("https://example.com", true);
        assert_eq!(manager.active_tab_id(), tab1);
        assert_eq!(
            manager.active_tab_needs_webview(),
            Some((tab1, String::from("https://example.com")))
        );

        // Creating a background tab does not switch active tab or create a WebView.
        let tab2 = manager.create_tab("https://rust-lang.org", false);
        assert_eq!(manager.active_tab_id(), tab1);
        assert!(manager.get_tab(tab2).is_some_and(|t| t.webview.is_none()));

        assert!(manager.switch_to(tab2));
        assert_eq!(manager.active_tab_id(), tab2);
        assert_eq!(
            manager.active_tab_needs_webview(),
            Some((tab2, String::from("https://rust-lang.org")))
        );
    }

    #[test]
    fn test_reorder_and_duplicate_tabs() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("https://one.example.com", true);
        let t2 = manager.create_tab("https://two.example.com", true);
        let t3 = manager.create_tab("https://three.example.com", true);

        manager.update_title(t2, String::from("Two Title"));
        manager.update_favicon(t2, Some(String::from("https://two.example.com/icon.png")));
        manager.update_scroll_position(t2, 12.0, 340.0);

        // Duplicate t2 right after t2
        let dup_id = manager.duplicate_tab(t2, true);
        assert!(dup_id.is_some());
        let dup_id = dup_id.unwrap_or(0);

        let ids: Vec<usize> = manager.summaries().iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![t1, t2, dup_id, t3]);

        let dup_tab = manager.get_tab(dup_id);
        assert!(dup_tab.is_some());
        if let Some(dup) = dup_tab {
            assert_eq!(dup.title, "Two Title");
            assert_eq!(dup.url, "https://two.example.com");
            assert_eq!(
                dup.favicon_url.as_deref(),
                Some("https://two.example.com/icon.png")
            );
            assert_eq!(dup.scroll_position, (12.0, 340.0));
            assert!(dup.pending_scroll_restore);
        }

        // Drag/reorder t3 to the front (position of t1)
        assert!(manager.reorder_tab(t3, t1));
        let reordered_ids: Vec<usize> = manager.summaries().iter().map(|s| s.id).collect();
        assert_eq!(reordered_ids, vec![t3, t1, t2, dup_id]);
    }

    #[test]
    fn test_close_and_reopen_recently_closed_stack() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("https://first.example.com", true);
        let t2 = manager.create_tab("https://second.example.com", true);

        manager.update_title(t2, String::from("Second Page"));
        manager.update_favicon(t2, Some(String::from("https://second.example.com/fav.ico")));
        manager.update_scroll_position(t2, 0.0, 512.0);

        assert_eq!(manager.close_tab(t2), CloseTabOutcome::Closed);
        assert_eq!(manager.active_tab_id(), t1);
        assert_eq!(manager.recently_closed().len(), 1);

        let reopened_id = manager.reopen_closed_tab();
        assert!(reopened_id.is_some());
        let reopened_id = reopened_id.unwrap_or(0);
        assert_eq!(manager.active_tab_id(), reopened_id);
        assert!(manager.recently_closed().is_empty());

        if let Some(reopened) = manager.get_tab(reopened_id) {
            assert_eq!(reopened.title, "Second Page");
            assert_eq!(reopened.url, "https://second.example.com");
            assert_eq!(
                reopened.favicon_url.as_deref(),
                Some("https://second.example.com/fav.ico")
            );
            assert_eq!(reopened.scroll_position, (0.0, 512.0));
        }
        assert_eq!(
            manager.take_pending_scroll_restore(reopened_id),
            Some((0.0, 512.0))
        );
        assert_eq!(manager.take_pending_scroll_restore(reopened_id), None);
    }

    #[test]
    fn test_background_tab_suspension_and_restore() {
        let timeout = Duration::from_secs(120);
        let mut manager = TabManager::new(timeout);

        let bg_tab = manager.create_tab("https://docs.rs", true);
        manager.update_title(bg_tab, String::from("Docs.rs"));
        manager.update_favicon(bg_tab, Some(String::from("https://docs.rs/favicon.ico")));
        manager.update_scroll_position(bg_tab, 24.0, 880.0);

        let active_tab = manager.create_tab("https://crates.io", true);
        assert_eq!(manager.active_tab_id(), active_tab);

        // Before timeout: nothing suspended
        let now = Instant::now();
        assert!(manager
            .suspend_inactive_tabs(now + Duration::from_secs(60))
            .is_empty());

        // After timeout: background tab is suspended while active tab is untouched
        let suspended = manager.suspend_inactive_tabs(now + Duration::from_secs(125));
        assert_eq!(suspended, vec![bg_tab]);

        if let Some(tab) = manager.get_tab(bg_tab) {
            assert!(tab.is_suspended);
            assert_eq!(tab.title, "Docs.rs");
            assert_eq!(tab.url, "https://docs.rs");
            assert_eq!(
                tab.favicon_url.as_deref(),
                Some("https://docs.rs/favicon.ico")
            );
            assert_eq!(tab.scroll_position, (24.0, 880.0));
        }

        // Clicking / switching to the suspended tab wakes it up and schedules scroll restoration
        assert!(manager.switch_to(bg_tab));
        if let Some(tab) = manager.get_tab(bg_tab) {
            assert!(!tab.is_suspended);
            assert!(tab.pending_scroll_restore);
        }
        assert_eq!(
            manager.active_tab_needs_webview(),
            Some((bg_tab, String::from("https://docs.rs")))
        );
    }

    #[test]
    fn test_open_or_focus_existing_internal_page() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let main_tab = manager.create_tab("https://example.com", true);
        assert_eq!(manager.summaries().len(), 1);
        assert_eq!(manager.active_tab_id(), main_tab);

        // Opening ember://settings opens a new tab with title "Settings" and settings favicon
        let (settings_tab, created) = manager.open_or_focus_internal_page("ember://settings");
        assert!(created);
        assert_ne!(settings_tab, main_tab);
        assert_eq!(manager.summaries().len(), 2);
        assert_eq!(manager.active_tab_id(), settings_tab);
        let tab = manager.get_tab(settings_tab).expect("settings tab");
        assert_eq!(tab.title, "Settings");
        assert_eq!(tab.url, "ember://settings");
        assert!(tab
            .favicon_url
            .as_deref()
            .is_some_and(|u| u.starts_with("data:image/svg+xml")));

        // Original tab remains intact
        assert_eq!(
            manager.get_tab(main_tab).map(|t| t.url.as_str()),
            Some("https://example.com")
        );

        // Switch back to main_tab, then request ember://settings again (even with trailing slash/casing)
        assert!(manager.switch_to(main_tab));
        assert_eq!(manager.active_tab_id(), main_tab);

        let (focused_id, created_again) = manager.open_or_focus_internal_page("EMBER://settings/");
        assert!(!created_again);
        assert_eq!(focused_id, settings_tab);
        assert_eq!(manager.summaries().len(), 2);
        assert_eq!(manager.active_tab_id(), settings_tab);

        // Same singleton behavior for history, bookmarks, and downloads
        let (hist_id, c1) = manager.open_or_focus_internal_page("ember://history");
        let (book_id, c2) = manager.open_or_focus_internal_page("ember://bookmarks");
        let (dl_id, c3) = manager.open_or_focus_internal_page("ember://downloads");
        assert!(c1 && c2 && c3);
        assert_eq!(manager.summaries().len(), 5);

        assert_eq!(
            manager.open_or_focus_internal_page("ember://history"),
            (hist_id, false)
        );
        assert_eq!(
            manager.open_or_focus_internal_page("ember://bookmarks"),
            (book_id, false)
        );
        assert_eq!(
            manager.open_or_focus_internal_page("ember://downloads"),
            (dl_id, false)
        );
        assert_eq!(manager.summaries().len(), 5);

        // Closing the Settings tab works normally
        assert_eq!(manager.close_tab(settings_tab), CloseTabOutcome::Closed);
        assert_eq!(manager.summaries().len(), 4);
    }

    #[test]
    fn test_close_unknown_tab_is_not_found() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("https://example.com", true);
        assert_eq!(manager.close_tab(t1 + 100), CloseTabOutcome::NotFound);
        assert_eq!(manager.summaries().len(), 1);
        assert!(manager.recently_closed().is_empty());
        assert_eq!(manager.active_tab_id(), t1);
    }

    #[test]
    fn test_popup_allowed_requires_recent_unused_activation() {
        let now = Instant::now();
        let window = POPUP_ACTIVATION_WINDOW;
        // No activation: blocked.
        assert!(!popup_allowed(None, None, now));
        // Fresh activation: allowed.
        let act = now;
        assert!(popup_allowed(
            Some(act),
            None,
            act + Duration::from_millis(200)
        ));
        // Stale activation: blocked.
        assert!(!popup_allowed(
            Some(act),
            None,
            act + window + Duration::from_millis(1)
        ));
        // Activation already consumed by an earlier popup: blocked.
        assert!(!popup_allowed(
            Some(act),
            Some(act + Duration::from_millis(10)),
            act + Duration::from_millis(20)
        ));
        // A popup from before the newest activation does not block.
        let act2 = act + Duration::from_millis(500);
        assert!(popup_allowed(
            Some(act2),
            Some(act + Duration::from_millis(10)),
            act2 + Duration::from_millis(5)
        ));
    }

    #[test]
    fn test_try_consume_popup_one_per_activation() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("https://example.com", true);
        let now = Instant::now();
        assert!(!manager.try_consume_popup(t1, now));
        assert!(manager.note_user_activation(t1, now));
        assert!(manager.try_consume_popup(t1, now + Duration::from_millis(50)));
        assert!(!manager.try_consume_popup(t1, now + Duration::from_millis(60)));
        assert!(!manager.try_consume_popup(t1 + 1, now));
    }

    #[test]
    fn test_typed_url_marked_once_with_final_url() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("ember://newtab", true);
        assert!(manager.set_pending_typed_url(t1, Some(String::from("https://x.com"))));

        // Still loading: nothing to mark yet.
        manager.update_page_load(t1, String::from("https://x.com"), true);
        assert_eq!(manager.take_typed_url_on_load_finished(t1), None);

        // Load finished after a redirect: the final URL is marked, exactly once.
        manager.update_page_load(t1, String::from("https://x.com/home"), false);
        assert_eq!(
            manager.take_typed_url_on_load_finished(t1),
            Some(String::from("https://x.com/home"))
        );
        assert_eq!(manager.take_typed_url_on_load_finished(t1), None);

        // Loads without a typed URL never mark anything.
        manager.update_page_load(t1, String::from("https://x.com/other"), false);
        assert_eq!(manager.take_typed_url_on_load_finished(t1), None);
    }

    #[test]
    fn test_spa_visit_dedup_decision() {
        assert!(!should_record_spa_visit(
            "https://app.example.com/a",
            "https://app.example.com/a"
        ));
        assert!(!should_record_spa_visit(
            "https://app.example.com/a",
            "https://app.example.com/a#section"
        ));
        assert!(should_record_spa_visit(
            "https://app.example.com/a",
            "https://app.example.com/b"
        ));
        assert!(should_record_spa_visit(
            "https://app.example.com/a?p=1",
            "https://app.example.com/a?p=2"
        ));
    }

    #[test]
    fn test_blocked_notice_keeps_url_on_synthetic_load() {
        let mut manager = TabManager::new(Duration::from_secs(60));
        let t1 = manager.create_tab("https://ads.example.com/x", true);
        if let Some(tab) = manager.get_tab_mut(t1) {
            tab.showing_blocked_notice = true;
        }
        manager.update_page_load(t1, String::from("about:blank"), false);
        assert_eq!(
            manager.get_tab(t1).map(|t| t.url.as_str()),
            Some("https://ads.example.com/x")
        );
        // A real navigation clears the notice flag.
        manager.update_page_load(t1, String::from("https://example.org/"), false);
        assert!(manager
            .get_tab(t1)
            .is_some_and(|t| !t.showing_blocked_notice && t.url == "https://example.org/"));
    }
}
