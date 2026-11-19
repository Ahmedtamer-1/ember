use std::{
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use wry::{PageLoadEvent, Rect, WebContext, WebView, WebViewBuilder};

use crate::{
    adblock::{self, Adblocker},
    context_menu,
    downloads::{DownloadManager, DownloadStatus},
    find::{self, FindState},
    pages::{self, PageCtx},
    permissions::{self, PermissionManager},
    privacy,
    settings::{SearchEngine, SettingsStore, ThemePreference},
    storage::Storage,
    tabs::{derive_default_favicon, CloseTabOutcome, TabManager},
    ui::{self, ChromeSyncState, PageIpcEvent, UiCommand, UiConfig},
    zoom,
};

/// Interval for checking whether inactive background tabs should be suspended.
const SUSPEND_CHECK_INTERVAL: Duration = Duration::from_secs(5);

struct BenchStepResult {
    site_name: &'static str,
    adblock_on: bool,
    load_ms: u128,
    ram_mb: f64,
    blocked_requests: usize,
}

struct BenchRunner {
    steps: Vec<(&'static str, &'static str, bool)>,
    current_idx: usize,
    step_started_at: Instant,
    step_finished_load_at: Option<Instant>,
    results: Vec<BenchStepResult>,
}

impl BenchRunner {
    fn from_env() -> Option<Self> {
        let mode = std::env::var("EMBER_BENCH_NEWS").ok()?;
        let steps = match mode.as_str() {
            "off" => vec![
                ("CNN", "https://edition.cnn.com/?ember_bench=off", false),
                (
                    "The Guardian",
                    "https://www.theguardian.com/international?ember_bench=off",
                    false,
                ),
                ("NYPost", "https://nypost.com/?ember_bench=off", false),
            ],
            "on" => vec![
                ("CNN", "https://edition.cnn.com/?ember_bench=on", true),
                (
                    "The Guardian",
                    "https://www.theguardian.com/international?ember_bench=on",
                    true,
                ),
                ("NYPost", "https://nypost.com/?ember_bench=on", true),
            ],
            "1" => vec![
                ("CNN", "https://edition.cnn.com/?ember_bench=off", false),
                (
                    "The Guardian",
                    "https://www.theguardian.com/international?ember_bench=off",
                    false,
                ),
                ("NYPost", "https://nypost.com/?ember_bench=off", false),
                ("CNN", "https://edition.cnn.com/?ember_bench=on", true),
                (
                    "The Guardian",
                    "https://www.theguardian.com/international?ember_bench=on",
                    true,
                ),
                ("NYPost", "https://nypost.com/?ember_bench=on", true),
            ],
            _ => return None,
        };
        Some(Self {
            steps,
            current_idx: 0,
            step_started_at: Instant::now(),
            step_finished_load_at: None,
            results: Vec::new(),
        })
    }

    fn write_report(&self) {
        let mut out = String::from(
            "# News Sites Adblock Benchmark (Windows 11 WebView2)\n\n\
             | Site | Mode | Page Load Time (ms) | Working Set RAM (MiB) | Blocked Requests |\n\
             | :--- | :--- | ---: | ---: | ---: |\n",
        );
        for r in &self.results {
            let mode = if r.adblock_on {
                "Adblock ON"
            } else {
                "Adblock OFF"
            };
            out.push_str(&format!(
                "| {} | {} | {} ms | {:.1} MiB | {} |\n",
                r.site_name, mode, r.load_ms, r.ram_mb, r.blocked_requests
            ));
        }
        println!("\n{out}");
        let _ = std::fs::write("target/news_benchmark.txt", out);
    }
}

/// Internal events dispatched from WebView callbacks to the main `tao` event loop.
#[derive(Debug, Clone)]
pub(crate) enum AppEvent {
    /// Periodic wake-up tick used during automated benchmark runs.
    Tick,
    /// Raw IPC command string received from the top Chrome UI WebView.
    UiIpc(String),
    /// Raw IPC message emitted inside a tab's content WebView. `source_uri` is the document URI
    /// reported by the webview for the sender (used to reject privileged messages from web pages).
    PageIpc {
        tab_id: usize,
        payload: String,
        source_uri: String,
    },
    /// Document title updated inside a tab's page WebView.
    TabTitleChanged { tab_id: usize, title: String },
    /// Page load started or finished inside a tab's page WebView.
    TabPageLoad {
        tab_id: usize,
        url: String,
        is_loading: bool,
    },
    /// A network request was blocked by the built-in adblocker inside `tab_id`.
    AdBlocked { tab_id: usize },
    /// A page in `tab_id` requested opening a URL in a new window/tab (`target="_blank"` or
    /// `window.open`). Subject to the popup blocker.
    OpenInNewTab { tab_id: usize, url: String },
    /// A file download started or completed.
    DownloadUpdated,
    /// HTTPS-only mode intercepted a navigation to an insecure `http://` URL inside `tab_id`.
    HttpsBlocked { tab_id: usize, url: String },
    /// A background task changed state that `ember://settings` displays (e.g. filter update done).
    RefreshSettingsPage,
    /// Fallback timer (~300ms) to reveal a newly created tab WebView if first paint hasn't arrived yet.
    TabRevealTimeout { tab_id: usize },
    /// Debug startup self-check timer (~2s) verifying the UI modules sent `ui_ready`.
    UiReadyTimeout,
    /// Self-test watchdog timer (~25s) in `--selftest` mode.
    SelfTestTimeout,
    /// Ranked omnibox suggestions and optional inline autofill computed from the in-memory cache.
    OmniboxSuggestions {
        query_id: u64,
        autofill: Option<crate::storage::AutofillMatch>,
        suggestions: Vec<crate::storage::OmniboxSuggestion>,
    },
}

struct WebViewEnv<'a> {
    window: &'a Window,
    web_context: &'a mut WebContext,
    storage: &'a Storage,
    adblocker: &'a Adblocker,
    permissions: &'a Arc<PermissionManager>,
    settings: &'a SettingsStore,
    downloads: &'a Arc<Mutex<DownloadManager>>,
    proxy: &'a EventLoopProxy<AppEvent>,
    content_bounds: Rect,
}

impl<'a> WebViewEnv<'a> {
    fn page_ctx(&self) -> PageCtx<'a> {
        PageCtx {
            storage: self.storage,
            settings: self.settings,
            adblocker: self.adblocker,
        }
    }
}

/// Ensures the currently active tab has a live `WebView` materialized.
/// Supports both lazy tab creation (`webview: None` until first selected) and
/// restoring suspended background tabs on click.
fn ensure_active_tab_webview(
    tabs: &mut TabManager,
    env: &mut WebViewEnv<'_>,
) -> Result<bool, wry::Error> {
    let Some((tab_id, mut url)) = tabs.active_tab_needs_webview() else {
        return Ok(false);
    };

    // HTTPS-only mode also applies to restored / reopened / duplicated tabs.
    if env.settings.should_intercept_http(&url) {
        url = pages::warning_url_for(&url);
    }

    let current_settings = env.settings.get();
    let adblock_clone = env.adblocker.clone();
    let nav_settings = env.settings.clone();
    let page_ipc_proxy = env.proxy.clone();
    let title_proxy = env.proxy.clone();
    let load_proxy = env.proxy.clone();
    let popup_proxy = env.proxy.clone();
    let nav_proxy = env.proxy.clone();
    let dl_start_proxy = env.proxy.clone();
    let dl_done_proxy = env.proxy.clone();
    let dl_start_mgr = Arc::clone(env.downloads);
    let dl_done_mgr = Arc::clone(env.downloads);
    let downloads_dir = env.storage.downloads_dir().to_path_buf();
    let ctx_menu_script = context_menu::context_menu_init_script();

    let mut builder = WebViewBuilder::with_web_context(env.web_context)
        .with_bounds(env.content_bounds)
        .with_visible(false)
        .with_background_color(ui::theme_background_rgba(
            current_settings.theme,
            env.window,
        ))
        .with_clipboard(true)
        .with_hotkeys_zoom(false)
        .with_devtools(cfg!(debug_assertions))
        .with_initialization_script(ui::PAGE_INIT_SCRIPT)
        .with_initialization_script(find::FIND_INIT_SCRIPT)
        .with_initialization_script(&ctx_menu_script);
    if current_settings.send_dnt_and_gpc {
        builder = builder.with_initialization_script(privacy::DNT_GPC_SCRIPT);
    }

    let webview = builder
        .with_ipc_handler(move |request| {
            let _ = page_ipc_proxy.send_event(AppEvent::PageIpc {
                tab_id,
                payload: request.body().clone(),
                source_uri: request.uri().to_string(),
            });
        })
        .with_navigation_handler(move |nav_url: String| {
            if adblock_clone.should_block(&nav_url) {
                return false;
            }
            if nav_settings.should_intercept_http(&nav_url) {
                let _ = nav_proxy.send_event(AppEvent::HttpsBlocked {
                    tab_id,
                    url: nav_url,
                });
                return false;
            }
            true
        })
        .with_document_title_changed_handler(move |title: String| {
            let _ = title_proxy.send_event(AppEvent::TabTitleChanged { tab_id, title });
        })
        .with_on_page_load_handler(move |event: PageLoadEvent, loaded_url: String| {
            let is_loading = matches!(event, PageLoadEvent::Started);
            let _ = load_proxy.send_event(AppEvent::TabPageLoad {
                tab_id,
                url: loaded_url,
                is_loading,
            });
        })
        .with_new_window_req_handler(move |target_url: String| {
            let _ = popup_proxy.send_event(AppEvent::OpenInNewTab {
                tab_id,
                url: target_url,
            });
            false
        })
        .with_download_started_handler(move |dl_url: String, default_path: &mut PathBuf| {
            if let Ok(mut mgr) = dl_start_mgr.lock() {
                let target = mgr.register_download_started(&dl_url, default_path, &downloads_dir);
                *default_path = target;
            }
            let _ = dl_start_proxy.send_event(AppEvent::DownloadUpdated);
            true
        })
        .with_download_completed_handler(
            move |dl_url: String, path: Option<PathBuf>, success: bool| {
                if let Ok(mut mgr) = dl_done_mgr.lock() {
                    mgr.mark_download_completed(&dl_url, path, success);
                }
                let _ = dl_done_proxy.send_event(AppEvent::DownloadUpdated);
            },
        )
        .build_as_child(env.window)?;

    let blocked_proxy = env.proxy.clone();
    adblock::attach_webview_adblocker(&webview, tab_id, env.adblocker.clone(), move || {
        let _ = blocked_proxy.send_event(AppEvent::AdBlocked { tab_id });
    });
    permissions::attach_webview_permissions(&webview, Arc::clone(env.permissions));

    // Apply active theme to the new webview
    ui::apply_webview_theme(&webview, current_settings.theme);

    // Load either internal `ember://` HTML or external URL BEFORE zooming so WebView2's
    // per-origin zoom never mutates `about:blank` (shared with the UI WebView).
    let page_ctx = env.page_ctx();
    load_target_into_webview(&webview, &url, &page_ctx);
    let zoom_factor = if pages::is_internal_url(&url) {
        1.0
    } else {
        resolve_zoom_for_url(env.storage, &url)
    };
    let _ = webview.zoom(zoom_factor);

    tabs.attach_webview(tab_id, webview);
    ui::bring_chrome_webview_to_front(env.window);
    let reveal_proxy = env.proxy.clone();
    let _ = thread::Builder::new()
        .name(String::from("ember-tab-reveal"))
        .spawn(move || {
            thread::sleep(Duration::from_millis(300));
            let _ = reveal_proxy.send_event(AppEvent::TabRevealTimeout { tab_id });
        });
    if pages::is_internal_url(&url) {
        if let Some((title, _)) = pages::render_internal_page(&url, &page_ctx) {
            tabs.update_title(tab_id, title);
        }
        if let Some(tab) = tabs.active_tab_mut() {
            tab.is_loading = false;
        }
    }
    Ok(true)
}

fn load_target_into_webview(webview: &WebView, url: &str, ctx: &PageCtx<'_>) {
    if let Some((_, html)) = pages::render_internal_page(url, ctx) {
        let _ = webview.load_html(&html);
    } else if !url.is_empty() {
        let _ = webview.load_url(url);
    }
}

fn resolve_zoom_for_url(storage: &Storage, url: &str) -> f64 {
    zoom::extract_host(url)
        .map(|host| storage.get_site_zoom(&host))
        .unwrap_or(1.0)
}

/// Escapes text for safe inclusion in HTML element content / attribute values.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Minimal inline page shown when an omnibox navigation targets a URL blocked by the ad blocker.
fn blocked_notice_html(url: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Blocked</title>\
         <style>body{{font-family:system-ui,'Segoe UI',sans-serif;margin:0;min-height:100vh;\
         display:flex;align-items:center;justify-content:center;background:#f8f9fa;color:#202124}}\
         @media (prefers-color-scheme: dark){{body{{background:#202124;color:#e8eaed}}}}\
         main{{max-width:640px;padding:24px}}h1{{font-size:20px;font-weight:600;margin:0 0 12px}}\
         code{{word-break:break-all;opacity:.85}}</style></head><body><main>\
         <h1>Blocked by Ember's ad blocker</h1><p><code>{}</code></p>\
         <p>This address matches a filter list rule. Pause the shield to open it.</p>\
         </main></body></html>",
        html_escape(url)
    )
}

/// Shows the inline "blocked by the ad blocker" notice in the active tab for `target_url`.
fn show_blocked_notice(tabs: &mut TabManager, env: &mut WebViewEnv<'_>, target_url: &str) {
    if tabs.active_tab_needs_webview().is_some() {
        // Materialize with a blank document instead of the tab's previous URL.
        if let Some(active) = tabs.active_tab_mut() {
            active.url = String::from("about:blank");
            active.pending_scroll_restore = false;
        }
        let _ = ensure_active_tab_webview(tabs, env);
    }
    let html = blocked_notice_html(target_url);
    if let Some(active) = tabs.active_tab_mut() {
        active.url = target_url.to_owned();
        active.title = String::from("Blocked");
        active.favicon_url = None;
        active.is_loading = false;
        active.showing_blocked_notice = true;
        active.scroll_position = (0.0, 0.0);
        active.pending_scroll_restore = false;
        if let Some(webview) = &active.webview {
            let _ = webview.zoom(1.0);
            let _ = webview.load_html(&html);
            let _ = webview.focus();
        }
    }
}

/// Navigates the active tab to `raw_input` (omnibox-style input). Returns `true` if the active
/// tab started loading the target (not redirected to a singleton page and not blocked).
fn navigate_active_tab(
    tabs: &mut TabManager,
    env: &mut WebViewEnv<'_>,
    find_state: &mut FindState,
    raw_input: &str,
) -> bool {
    // `ember://warning?url=...` must keep its query (normalization canonicalizes it away).
    let mut target_url = if pages::is_internal_url(raw_input)
        && pages::canonical_internal_url(raw_input) == Some(pages::WARNING_URL)
        && pages::warning_target(raw_input).is_some()
    {
        raw_input.trim().to_owned()
    } else {
        ui::normalize_omnibox_input(raw_input, env.settings.get().search_engine)
    };
    if let Some(canonical) = pages::is_singleton_internal_url(&target_url) {
        tabs.open_or_focus_internal_page(canonical);
        let _ = ensure_active_tab_webview(tabs, env);
        return false;
    }
    if env.settings.should_intercept_http(&target_url) {
        target_url = pages::warning_url_for(&target_url);
    }
    let active_id = tabs.active_tab_id();
    tabs.set_pending_typed_url(active_id, None);

    find_state.active_match = 0;
    find_state.total_matches = 0;

    if !pages::is_internal_url(&target_url) && env.adblocker.should_block(&target_url) {
        show_blocked_notice(tabs, env, &target_url);
        return false;
    }

    tabs.reset_blocked_count(active_id);

    if tabs.active_tab_needs_webview().is_some() {
        // Suspended / lazily-created tab: point it at the new target BEFORE materializing so
        // only the new URL is loaded (not the stale one first).
        if let Some(active) = tabs.active_tab_mut() {
            active.url.clone_from(&target_url);
            active.favicon_url = derive_default_favicon(&target_url);
            active.scroll_position = (0.0, 0.0);
            active.pending_scroll_restore = false;
            active.showing_blocked_notice = false;
        }
        let _ = ensure_active_tab_webview(tabs, env);
        if let Some(webview) = tabs.active_tab().and_then(|t| t.webview.as_ref()) {
            let _ = webview.focus();
        }
        return true;
    }

    let zoom_factor = resolve_zoom_for_url(env.storage, &target_url);
    let page_ctx = env.page_ctx();

    if let Some(active) = tabs.active_tab_mut() {
        active.url.clone_from(&target_url);
        active.favicon_url = derive_default_favicon(&target_url);
        active.scroll_position = (0.0, 0.0);
        active.showing_blocked_notice = false;

        if let Some((title, html)) = pages::render_internal_page(&target_url, &page_ctx) {
            active.title = title;
            active.is_loading = false;
            if let Some(webview) = &active.webview {
                let _ = webview.zoom(1.0);
                let _ = webview.load_html(&html);
                let _ = webview.focus();
            }
        } else {
            active.is_loading = true;
            if let Some(webview) = &active.webview {
                let _ = webview.load_url(&target_url);
                let _ = webview.zoom(zoom_factor);
                let _ = webview.focus();
            }
        }
    }
    true
}

fn reload_active_tab(
    tabs: &mut TabManager,
    storage: &Storage,
    settings: &SettingsStore,
    adblocker: &Adblocker,
) {
    let active_id = tabs.active_tab_id();
    reload_tab(tabs, active_id, storage, settings, adblocker);
}

/// Reloads `tab_id` (re-rendering internal pages / the blocked notice from current state).
fn reload_tab(
    tabs: &mut TabManager,
    tab_id: usize,
    storage: &Storage,
    settings: &SettingsStore,
    adblocker: &Adblocker,
) {
    if let Some(tab) = tabs.get_tab_mut(tab_id) {
        tab.blocked_count = 0;
        let url = tab.url.clone();
        let page_ctx = PageCtx {
            storage,
            settings,
            adblocker,
        };
        if tab.showing_blocked_notice {
            if adblocker.should_block(&url) {
                if let Some(webview) = &tab.webview {
                    let _ = webview.load_html(&blocked_notice_html(&url));
                }
            } else {
                tab.showing_blocked_notice = false;
                tab.is_loading = true;
                if let Some(webview) = &tab.webview {
                    let _ = webview.load_url(&url);
                }
            }
        } else if let Some((title, html)) = pages::render_internal_page(&url, &page_ctx) {
            tab.title = title;
            tab.is_loading = false;
            if let Some(webview) = &tab.webview {
                let _ = webview.load_html(&html);
            }
        } else if let Some(webview) = &tab.webview {
            let _ = webview.reload();
        }
    }
}

/// Opens `raw_input` (omnibox-style input, e.g. the homepage or a CLI argument) in a new tab,
/// normalizing it first. Singleton internal pages are focused instead of duplicated when
/// `activate` is set. Returns the tab ID.
fn open_input_in_new_tab(
    tabs: &mut TabManager,
    raw_input: &str,
    engine: SearchEngine,
    activate: bool,
) -> usize {
    let normalized = ui::normalize_omnibox_input(raw_input, engine);
    if activate {
        if let Some(canonical) = pages::is_singleton_internal_url(&normalized) {
            return tabs.open_or_focus_internal_page(canonical).0;
        }
    }
    tabs.create_tab(&normalized, activate)
}

/// Resets per-tab find state when the active tab changes. Returns `true` if the previous tab
/// may still show find highlights that should be cleared.
fn reset_find_state_for_tab_change(
    find_state: &mut FindState,
    prev_tab: usize,
    new_tab: usize,
) -> bool {
    if prev_tab == new_tab {
        return false;
    }
    let had_find =
        find_state.is_open || find_state.total_matches > 0 || find_state.active_match > 0;
    find_state.is_open = false;
    find_state.active_match = 0;
    find_state.total_matches = 0;
    had_find
}

fn adjust_active_tab_zoom<F>(tabs: &TabManager, storage: &Storage, f: F)
where
    F: FnOnce(f64) -> f64,
{
    let Some(active) = tabs.active_tab() else {
        return;
    };
    let Some(host) = zoom::extract_host(&active.url) else {
        return;
    };
    let current = storage.get_site_zoom(&host);
    let next = f(current);
    storage.set_site_zoom(&host, next);
    if let Some(webview) = &active.webview {
        let _ = webview.zoom(next);
    }
}

/// Synchronizes the top Chrome UI WebView and native window title with the current browser state.
#[allow(clippy::too_many_arguments)]
fn sync_chrome_ui(
    chrome_webview: &WebView,
    window: &Window,
    tabs: &TabManager,
    storage: &Storage,
    adblocker: &Adblocker,
    settings: &SettingsStore,
    find_state: &mut FindState,
    downloads: &Arc<Mutex<DownloadManager>>,
) {
    // Find state is per tab: every active-tab change (switch/close/new/reopen/duplicate) ends
    // in a UI sync, so detect it here, clear the old tab's highlights and reset the find bar.
    static FIND_OWNER_TAB: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let active_id = tabs.active_tab_id();
    let prev_find_tab = FIND_OWNER_TAB.swap(active_id, std::sync::atomic::Ordering::Relaxed);
    if reset_find_state_for_tab_change(find_state, prev_find_tab, active_id) {
        if let Some(webview) = tabs.get_tab(prev_find_tab).and_then(|t| t.webview.as_ref()) {
            let _ = webview.evaluate_script(find::build_clear_find_script());
        }
    }

    let summaries = tabs.summaries();
    let appearance = {
        let mut app = ui::query_window_appearance(window);
        app.is_dark = settings.get().theme.resolve_is_dark(app.is_dark);
        app
    };

    let (active_site_allowlisted, active_bookmarked, active_zoom_percent) =
        if let Some(active) = tabs.active_tab() {
            let allowlisted = adblocker.is_site_allowlisted(&active.url);
            let bookmarked = storage.is_bookmarked(&active.url);
            let factor = resolve_zoom_for_url(storage, &active.url);
            (allowlisted, bookmarked, zoom::zoom_percent(factor))
        } else {
            (false, false, 100)
        };

    let (downloads_count, downloads_active, latest_download_text) =
        if let Ok(mgr) = downloads.lock() {
            let items = mgr.items();
            let count = items.len();
            let active_dl = items
                .iter()
                .filter(|d| d.status == DownloadStatus::InProgress)
                .count();
            let latest = items
                .first()
                .map(|d| {
                    format!(
                        "{} ({}) -> {}",
                        d.file_name,
                        d.status.as_str(),
                        d.save_path.display()
                    )
                })
                .unwrap_or_default();
            (count, active_dl, latest)
        } else {
            (0, 0, String::new())
        };

    static TAB_REVISION_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let tab_revision = TAB_REVISION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let sync_state = ChromeSyncState {
        tab_revision,
        active_tab_id: tabs.active_tab_id(),
        adblock_enabled: adblocker.is_enabled(),
        active_site_allowlisted,
        active_bookmarked,
        active_zoom_percent,
        find_open: find_state.is_open,
        find_active: find_state.active_match,
        find_total: find_state.total_matches,
        downloads_count,
        downloads_active,
        latest_download_text: &latest_download_text,
        recently_closed_count: tabs.recently_closed().len(),
        tab_animations_enabled: settings.get().tab_animations_enabled,
        appearance: &appearance,
        tabs: &summaries,
    };

    let _ = chrome_webview.zoom(1.0);
    let script = ui::build_state_sync_script(&sync_state);
    let _ = chrome_webview.evaluate_script(&script);

    if let Some(active) = tabs.active_tab() {
        let title = if active.title.trim().is_empty() {
            String::from("Ember")
        } else {
            format!("{} - Ember", active.title)
        };
        window.set_title(&title);
    } else {
        window.set_title("Ember");
    }
}

/// Runs the Ember browser application event loop.
pub fn run() -> Result<(), Box<dyn Error>> {
    let _webview_version = ui::ensure_webview_runtime()?;

    let args: Vec<String> = std::env::args().collect();
    let selftest_mode = args.iter().any(|a| a == "--selftest");
    #[cfg(debug_assertions)]
    let open_ui_devtools_on_start = args.iter().any(|a| a == "--ui-devtools")
        || std::env::var("EMBER_UI_DEVTOOLS")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));

    let _ = Storage::ensure_default_logs_dir();

    let selftest_temp_dir = if selftest_mode {
        let dir = std::env::temp_dir().join(format!("ember_selftest_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Some(dir)
    } else {
        None
    };

    let storage = match &selftest_temp_dir {
        Some(dir) => Storage::with_base_dir(dir.clone())?,
        None => Storage::init()?,
    };
    let settings = match &selftest_temp_dir {
        Some(dir) => SettingsStore::with_config_dir(dir.clone())?,
        None => SettingsStore::init()?,
    };
    if let Some(config_dir) = settings.config_path().parent() {
        crate::theme::init(config_dir);
    }
    let init_settings = settings.get();
    let adblocker = Adblocker::with_filters_dir(
        storage.filters_dir(),
        init_settings.easylist_enabled,
        init_settings.easyprivacy_enabled,
    );
    // Honour the persisted global toggle (bench mode overrides it per step below).
    adblocker.set_enabled(init_settings.adblock_enabled);
    adblocker.set_privacy_flags(
        init_settings.send_dnt_and_gpc,
        init_settings.block_third_party_cookies,
    );
    let permissions = Arc::new(PermissionManager::new());
    let downloads = Arc::new(Mutex::new(DownloadManager::new()));
    let mut find_state = FindState::default();
    let ui_config = UiConfig::new();
    let mut tabs = TabManager::new(init_settings.tab_suspend_timeout());

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let builder = WindowBuilder::new()
        .with_title(&ui_config.title)
        .with_inner_size(LogicalSize::new(ui_config.width, ui_config.height))
        .with_min_inner_size(LogicalSize::new(480.0, 320.0));
    let window = ui::configure_window_builder(builder).build(&event_loop)?;
    ui::install_window_frame(&window);
    ui::apply_theme_preference(&window, init_settings.theme);

    let (initial_chrome_bounds, mut current_content_bounds) =
        ui::compute_layout_bounds(window.inner_size(), window.scale_factor());

    let mut web_context = WebContext::new(Some(storage.webview_data_dir().to_path_buf()));

    let ipc_proxy = proxy.clone();
    let chrome_webview = WebViewBuilder::with_web_context(&mut web_context)
        .with_bounds(initial_chrome_bounds)
        .with_transparent(true)
        .with_hotkeys_zoom(false)
        .with_devtools(cfg!(debug_assertions))
        .with_html(crate::theme::inject_style(
            ui::CHROME_HTML,
            &crate::theme::current().chrome_css(),
        ))
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(AppEvent::UiIpc(request.body().clone()));
        })
        .build_as_child(&window)?;
    let _ = chrome_webview.zoom(1.0);
    ui::bring_chrome_webview_to_front(&window);
    ui::apply_webview_theme(&chrome_webview, init_settings.theme);

    #[cfg(debug_assertions)]
    if open_ui_devtools_on_start {
        chrome_webview.open_devtools();
    }

    let mut selftest_active = selftest_mode;
    let mut selftest_saw_popup_open = false;
    let mut selftest_saw_popup_close = false;
    let mut selftest_bounds_always_restored = true;
    let mut selftest_overlay_open_count: usize = 0;
    let mut selftest_overlay_close_count: usize = 0;
    let mut last_selftest_report_json: Option<String> = None;

    if selftest_mode {
        storage.record_visit(
            "https://selftest.ember.local/guide",
            "Ember Self-Test Guide",
        );
        let st_proxy = proxy.clone();
        let _ = thread::Builder::new()
            .name(String::from("ember-selftest-timeout"))
            .spawn(move || {
                thread::sleep(Duration::from_secs(25));
                let _ = st_proxy.send_event(AppEvent::SelfTestTimeout);
                // Hard watchdog: if the event loop is hung (e.g. stuck during startup), the
                // timeout event above is never processed. A finished self-test exits the
                // process before this fires.
                thread::sleep(Duration::from_secs(10));
                eprintln!("FAIL: self-test hung (event loop unresponsive after 35s)");
                std::process::exit(1);
            });
    }

    let mut ui_ready_received = false;
    let mut ui_failed_modules: Vec<(String, String)> = Vec::new();
    if cfg!(debug_assertions) {
        let ready_proxy = proxy.clone();
        let _ = thread::Builder::new()
            .name(String::from("ember-ui-ready-check"))
            .spawn(move || {
                thread::sleep(Duration::from_secs(2));
                let _ = ready_proxy.send_event(AppEvent::UiReadyTimeout);
            });
    }

    let mut bench_runner = BenchRunner::from_env();
    if bench_runner.is_some() {
        let tick_proxy = proxy.clone();
        let _ = thread::Builder::new()
            .name(String::from("ember-bench-ticker"))
            .spawn(move || loop {
                thread::sleep(Duration::from_millis(250));
                if tick_proxy.send_event(AppEvent::Tick).is_err() {
                    break;
                }
            });
    }

    let cli_urls: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    let initial_url = if let Some(runner) = &mut bench_runner {
        let (_, url, adblock_on) = runner.steps[0];
        adblocker.set_enabled(adblock_on);
        runner.step_started_at = Instant::now();
        url.to_string()
    } else if let Some(first) = cli_urls.first() {
        first.clone()
    } else {
        init_settings.homepage.clone()
    };

    open_input_in_new_tab(&mut tabs, &initial_url, init_settings.search_engine, true);

    if bench_runner.is_none() && cli_urls.len() > 1 {
        for url in &cli_urls[1..] {
            open_input_in_new_tab(&mut tabs, url, init_settings.search_engine, false);
        }
        if std::env::var("EMBER_BENCH_MATERIALIZE_ALL").is_ok() {
            let active_id = tabs.active_tab_id();
            let all_ids: Vec<usize> = tabs.summaries().into_iter().map(|s| s.id).collect();
            for id in all_ids {
                tabs.switch_to(id);
                let mut env = WebViewEnv {
                    window: &window,
                    web_context: &mut web_context,
                    storage: &storage,
                    adblocker: &adblocker,
                    permissions: &permissions,
                    settings: &settings,
                    downloads: &downloads,
                    proxy: &proxy,
                    content_bounds: current_content_bounds,
                };
                let _ = ensure_active_tab_webview(&mut tabs, &mut env);
            }
            tabs.switch_to(active_id);
        }
    }
    {
        let mut env = WebViewEnv {
            window: &window,
            web_context: &mut web_context,
            storage: &storage,
            adblocker: &adblocker,
            permissions: &permissions,
            settings: &settings,
            downloads: &downloads,
            proxy: &proxy,
            content_bounds: current_content_bounds,
        };
        ensure_active_tab_webview(&mut tabs, &mut env)?;
    }

    sync_chrome_ui(
        &chrome_webview,
        &window,
        &tabs,
        &storage,
        &adblocker,
        &settings,
        &mut find_state,
        &downloads,
    );

    let mut overlay_controller = crate::popup::OverlayController::new();

    let check_interval = tabs
        .suspend_timeout()
        .min(SUSPEND_CHECK_INTERVAL)
        .max(Duration::from_secs(1));

    event_loop.run(move |event, _window_target, control_flow| {
        let mut wait_slice = if bench_runner.is_some() {
            Duration::from_millis(250)
        } else {
            check_interval
        };
        if overlay_controller.is_expanded() {
            wait_slice = wait_slice.min(Duration::from_millis(50));
        }
        *control_flow = ControlFlow::WaitUntil(Instant::now() + wait_slice);

        if overlay_controller.is_expanded()
            && overlay_controller.check_watchdog(Instant::now(), &chrome_webview, &window)
            && !overlay_controller.is_at_toolbar_height(&chrome_webview, &window)
        {
            selftest_bounds_always_restored = false;
        }

        if let Some(runner) = &mut bench_runner {
            if runner.current_idx >= runner.steps.len() {
                *control_flow = ControlFlow::Exit;
                return;
            }
            let now = Instant::now();
            let elapsed_since_start = now.saturating_duration_since(runner.step_started_at);
            let settle_done = runner.step_finished_load_at.is_some_and(|fin| {
                now.saturating_duration_since(fin) >= Duration::from_millis(1500)
            });
            if settle_done || elapsed_since_start >= Duration::from_secs(9) {
                let (site_name, _, adblock_on) = runner.steps[runner.current_idx];
                let load_dur = runner
                    .step_finished_load_at
                    .map(|fin| fin.saturating_duration_since(runner.step_started_at))
                    .unwrap_or(elapsed_since_start);
                let blocked_requests = tabs.active_tab().map_or(0, |t| t.blocked_count);
                let ram_mb = adblock::measure_process_tree_ram_mb();
                runner.results.push(BenchStepResult {
                    site_name,
                    adblock_on,
                    load_ms: load_dur.as_millis(),
                    ram_mb,
                    blocked_requests,
                });

                runner.current_idx += 1;
                if runner.current_idx >= runner.steps.len() {
                    runner.write_report();
                    *control_flow = ControlFlow::Exit;
                    return;
                } else {
                    let (_, next_url, next_adblock_on) = runner.steps[runner.current_idx];
                    adblocker.set_enabled(next_adblock_on);
                    let prev_id = tabs.active_tab_id();
                    tabs.create_tab(next_url, true);
                    let _ = tabs.close_tab(prev_id);
                    let mut env = WebViewEnv {
                        window: &window,
                        web_context: &mut web_context,
                        storage: &storage,
                        adblocker: &adblocker,
                        permissions: &permissions,
                        settings: &settings,
                        downloads: &downloads,
                        proxy: &proxy,
                        content_bounds: current_content_bounds,
                    };
                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                    runner.step_started_at = Instant::now();
                    runner.step_finished_load_at = None;
                    sync_chrome_ui(
                        &chrome_webview,
                        &window,
                        &tabs,
                        &storage,
                        &adblocker,
                        &settings,
                        &mut find_state,
                        &downloads,
                    );
                }
            }
        }

        if !tabs.suspend_inactive_tabs(Instant::now()).is_empty() {
            sync_chrome_ui(
                &chrome_webview,
                &window,
                &tabs,
                &storage,
                &adblocker,
                &settings,
                &mut find_state,
                &downloads,
            );
        }

        match event {
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if window_id == window.id() => {
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Moved(_),
                ..
            } if window_id == window.id() => {
                collapse_overlay_from_host(&mut overlay_controller, "window_move", &chrome_webview, &window);
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Focused(false),
                ..
            } if window_id == window.id() => {
                if overlay_controller.is_expanded() && !ui::is_window_foreground(&window) {
                    collapse_overlay_from_host(&mut overlay_controller, "window_blur", &chrome_webview, &window);
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Resized(physical_size),
                ..
            } if window_id == window.id() => {
                collapse_overlay_from_host(&mut overlay_controller, "window_resize", &chrome_webview, &window);
                let (chrome_bounds, content_bounds) =
                    ui::compute_layout_bounds(physical_size, window.scale_factor());
                current_content_bounds = content_bounds;
                let _ = chrome_webview.set_bounds(chrome_bounds);
                let _ = chrome_webview.zoom(1.0);
                tabs.resize_all(content_bounds);
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &mut find_state,
                    &downloads,
                );
            }
            Event::WindowEvent {
                window_id,
                event:
                    WindowEvent::ScaleFactorChanged {
                        scale_factor,
                        new_inner_size,
                    },
                ..
            } if window_id == window.id() => {
                collapse_overlay_from_host(&mut overlay_controller, "dpi_change", &chrome_webview, &window);
                let (chrome_bounds, content_bounds) =
                    ui::compute_layout_bounds(*new_inner_size, scale_factor);
                current_content_bounds = content_bounds;
                let _ = chrome_webview.set_bounds(chrome_bounds);
                let _ = chrome_webview.zoom(1.0);
                tabs.resize_all(content_bounds);
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &mut find_state,
                    &downloads,
                );
            }
            Event::WindowEvent {
                event: WindowEvent::ThemeChanged(_),
                ..
            } => {
                ui::apply_window_chrome_style(&window);
                let theme_pref = settings.get().theme;
                ui::apply_webview_theme(&chrome_webview, theme_pref);
                let content_bg = ui::theme_background_rgba(theme_pref, &window);
                tabs.for_each_webview(|wv| {
                    let _ = wv.set_background_color(content_bg);
                });
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &mut find_state,
                    &downloads,
                );
            }
            Event::UserEvent(app_event) => match app_event {
                AppEvent::Tick => {}
                AppEvent::DownloadUpdated => {
                    sync_chrome_ui(
                        &chrome_webview,
                        &window,
                        &tabs,
                        &storage,
                        &adblocker,
                        &settings,
                        &mut find_state,
                        &downloads,
                    );
                }
                AppEvent::UiIpc(raw_msg) => {
                    if let Some(cmd) = ui::parse_ipc_command(&raw_msg) {
                        match cmd {
                            UiCommand::Ready => {
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::UiReady => {
                                ui_ready_received = true;
                                if selftest_mode {
                                    selftest_active = true;
                                    selftest_saw_popup_open = false;
                                    selftest_saw_popup_close = false;
                                    selftest_bounds_always_restored = true;
                                    selftest_overlay_open_count = 0;
                                    selftest_overlay_close_count = 0;
                                    window.set_focus();
                                    let _ = chrome_webview.focus();
                                    let _ = chrome_webview.evaluate_script(
                                        "if (typeof window.__emberRunSelfTest === 'function') { window.__emberRunSelfTest(); }",
                                    );
                                }
                            }
                            UiCommand::UiInitError { module, error } => {
                                eprintln!(
                                    "[ember][ui-self-check] UI module '{module}' failed to initialize: {error}"
                                );
                                storage.log_ui_js_error(
                                    "ui_init_error",
                                    &format!("{module}: {error}"),
                                );
                                ui_failed_modules.push((module, error));
                            }
                            UiCommand::UiJsError(payload) => {
                                eprintln!("[ember][ui-js-error] {payload}");
                                storage.log_ui_js_error("ui", &payload);
                            }
                            UiCommand::OpenUiDevTools => {
                                #[cfg(debug_assertions)]
                                {
                                    chrome_webview.open_devtools();
                                }
                            }
                            UiCommand::SelfTestReport(report_json) => {
                                let mut parsed = parse_selftest_report(&report_json);
                                let rust_tab_count = tabs.summaries().len();
                                let popup_currently_open = overlay_controller.is_expanded();
                                let bounds_at_toolbar_now =
                                    overlay_controller.is_at_toolbar_height(&chrome_webview, &window);
                                if !bounds_at_toolbar_now {
                                    selftest_bounds_always_restored = false;
                                }

                                for item in &mut parsed.items {
                                    if item.name == "popup opens and closes" {
                                        let rust_popup_ok = selftest_saw_popup_open
                                            && selftest_saw_popup_close
                                            && !popup_currently_open
                                            && selftest_bounds_always_restored;
                                        item.pass = item.pass && rust_popup_ok;
                                        item.detail = format!(
                                            "{}; rustOpen={selftest_saw_popup_open} rustClosed={selftest_saw_popup_close} currentlyExpanded={popup_currently_open} opens={selftest_overlay_open_count} closes={selftest_overlay_close_count} boundsAlwaysRestored={selftest_bounds_always_restored}",
                                            item.detail
                                        );
                                    } else if item.name.contains("50 times") {
                                        let rust_stress_ok = selftest_overlay_open_count >= 450
                                            && selftest_overlay_close_count >= 450
                                            && selftest_bounds_always_restored
                                            && !popup_currently_open;
                                        item.pass = item.pass && rust_stress_ok;
                                        item.detail = format!(
                                            "{}; rustOpens={selftest_overlay_open_count} rustCloses={selftest_overlay_close_count} boundsAlwaysRestored={selftest_bounds_always_restored}",
                                            item.detail
                                        );
                                    } else if item.name == "DOM tab count equals Rust tab count" {
                                        let rust_count_match = parsed.dom_count
                                            == Some(rust_tab_count)
                                            && parsed.all_dom_count == Some(rust_tab_count);
                                        item.pass = item.pass && rust_count_match;
                                        item.detail = format!(
                                            "{}; rustTabs={rust_tab_count}",
                                            item.detail
                                        );
                                    }
                                    println!(
                                        "{}: {} ({})",
                                        if item.pass { "PASS" } else { "FAIL" },
                                        item.name,
                                        item.detail
                                    );
                                }

                                let all_passed = parsed.items.len() >= 9
                                    && parsed.items.iter().all(|item| item.pass);
                                let verified_json = build_selftest_report_json(&parsed.items);
                                last_selftest_report_json = Some(verified_json.clone());

                                let render_script = format!(
                                    "if (typeof window.__emberRenderSelfTestReport === 'function') {{ window.__emberRenderSelfTestReport({verified_json}); }}"
                                );
                                for summary in tabs.summaries() {
                                    if summary.url == pages::SELFTEST_URL {
                                        if let Some(tab) = tabs.get_tab(summary.id) {
                                            if let Some(wv) = &tab.webview {
                                                let _ = wv.evaluate_script(&render_script);
                                            }
                                        }
                                    }
                                }

                                selftest_active = false;
                                if selftest_mode {
                                    if let Some(ref dir) = selftest_temp_dir {
                                        let _ = std::fs::remove_dir_all(dir);
                                    }
                                    if all_passed {
                                        *control_flow = ControlFlow::Exit;
                                    } else {
                                        std::process::exit(1);
                                    }
                                }
                            }
                            UiCommand::NewTab => {
                                collapse_overlay_from_host(&mut overlay_controller, "new_tab", &chrome_webview, &window);
                                let s = settings.get();
                                open_input_in_new_tab(&mut tabs, &s.homepage, s.search_engine, true);
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                                if selftest_active {
                                    let _ = chrome_webview.focus();
                                }
                            }
                            UiCommand::NewWindow => {
                                // A second process would share settings.toml and the SQLite
                                // profile (last writer wins), so open a new tab instead.
                                eprintln!(
                                    "[ember] multiple windows are not supported yet; opening a new tab instead"
                                );
                                collapse_overlay_from_host(&mut overlay_controller, "new_window", &chrome_webview, &window);
                                let s = settings.get();
                                open_input_in_new_tab(&mut tabs, &s.homepage, s.search_engine, true);
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::OpenNewTab(target_url) => {
                                collapse_overlay_from_host(&mut overlay_controller, "open_new_tab", &chrome_webview, &window);
                                open_input_in_new_tab(
                                    &mut tabs,
                                    &target_url,
                                    settings.get().search_engine,
                                    true,
                                );
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::OpenDownloads => {
                                open_directory_in_shell(storage.downloads_dir());
                            }
                            UiCommand::Print => {
                                if let Some(webview) =
                                    tabs.active_tab().and_then(|t| t.webview.as_ref())
                                {
                                    let _ = webview.evaluate_script("window.print();");
                                }
                            }
                            UiCommand::OverlayExpand(kind_raw) => {
                                let kind = crate::popup::OverlayKind::parse(&kind_raw)
                                    .unwrap_or(crate::popup::OverlayKind::Omnibox);
                                let already_same = overlay_controller.is_expanded()
                                    && overlay_controller.active_kind() == Some(kind);
                                overlay_controller.expand(kind, &chrome_webview, &window);
                                selftest_saw_popup_open = true;
                                if !already_same {
                                    selftest_overlay_open_count += 1;
                                }
                                let expand_script = format!(
                                    "if(window.__emberOnOverlayExpanded){{window.__emberOnOverlayExpanded(\"{}\");}}",
                                    kind.as_str()
                                );
                                let _ = chrome_webview.evaluate_script(&expand_script);
                            }
                            UiCommand::OverlayShrink(reason) => {
                                storage.cancel_pending_queries();
                                overlay_controller.collapse(&reason, &chrome_webview, &window);
                                if !overlay_controller.is_at_toolbar_height(&chrome_webview, &window)
                                {
                                    selftest_bounds_always_restored = false;
                                }
                                selftest_saw_popup_close = true;
                                selftest_overlay_close_count += 1;
                                let _ = chrome_webview.evaluate_script(
                                    "if(window.__emberOnOverlayCollapsed){window.__emberOnOverlayCollapsed();}",
                                );
                            }
                            UiCommand::OmniboxFocus => {
                                // Do NOT call `chrome_webview.focus()` here: this IPC is sent from
                                // the omnibox `focus` event, and WebView2's MoveFocus on an
                                // already-focused controller bounces focus (blur -> focus), which
                                // re-fires this IPC and closes any open overlay via the blur handlers.
                                overlay_controller.on_omnibox_focus();
                            }
                            UiCommand::OmniboxBlur => {
                                overlay_controller.on_omnibox_blur(Instant::now());
                            }
                            UiCommand::OverlayVisibility { visible, kind } => {
                                overlay_controller.on_dropdown_visibility(
                                    visible,
                                    crate::popup::OverlayKind::parse(&kind),
                                    Instant::now(),
                                );
                            }
                            UiCommand::OmniboxSuggest {
                                query_id,
                                allow_autofill,
                                query,
                            } => {
                                crate::popup::popup_debug_log(&format!(
                                    "omnibox_suggest recv seq={query_id} autofill={allow_autofill} q={query:?}"
                                ));
                                let s = settings.get();
                                let (autofill, mut suggestions) =
                                    storage.query_omnibox_cached(&query, allow_autofill, 8);
                                ui::ensure_default_omnibox_row(
                                    &query,
                                    s.search_engine,
                                    &mut suggestions,
                                );
                                crate::popup::popup_debug_log(&format!(
                                    "omnibox_suggest reply seq={query_id} count={} autofill={}",
                                    suggestions.len(),
                                    autofill.is_some()
                                ));
                                let _ = proxy.send_event(AppEvent::OmniboxSuggestions {
                                    query_id,
                                    autofill: autofill.clone(),
                                    suggestions: suggestions.clone(),
                                });

                                if s.search_suggestions_enabled && suggestions.len() < 8 {
                                    let suggest_proxy = proxy.clone();
                                    let query_for_default = query.clone();
                                    storage.suggest_async(
                                        query_id,
                                        query,
                                        8,
                                        true,
                                        s.search_engine,
                                        move |qid, mut online_suggestions| {
                                            if let Some(ref af) = autofill {
                                                if let Some(pos) = online_suggestions
                                                    .iter()
                                                    .position(|item| item.url == af.full_url)
                                                {
                                                    let mut item = online_suggestions.remove(pos);
                                                    item.is_default = true;
                                                    online_suggestions.insert(0, item);
                                                } else {
                                                    online_suggestions.insert(
                                                        0,
                                                        crate::storage::OmniboxSuggestion {
                                                            kind: af.kind,
                                                            title: af.title.clone(),
                                                            url: af.full_url.clone(),
                                                            score: af.score + 2000.0,
                                                            is_default: true,
                                                        },
                                                    );
                                                    online_suggestions.truncate(8);
                                                }
                                            }
                                            ui::ensure_default_omnibox_row(
                                                &query_for_default,
                                                s.search_engine,
                                                &mut online_suggestions,
                                            );
                                            let _ = suggest_proxy.send_event(
                                                AppEvent::OmniboxSuggestions {
                                                    query_id: qid,
                                                    autofill,
                                                    suggestions: online_suggestions,
                                                },
                                            );
                                        },
                                    );
                                }
                            }
                            UiCommand::OmniboxSelect(_) => {}
                            UiCommand::SwitchTab(tab_id) => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "switch_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.switch_to(tab_id) {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::NextTab => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "next_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.switch_next_tab() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                    if selftest_active {
                                        let _ = chrome_webview.focus();
                                    }
                                }
                            }
                            UiCommand::PrevTab => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "prev_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.switch_prev_tab() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                    if selftest_active {
                                        let _ = chrome_webview.focus();
                                    }
                                }
                            }
                            UiCommand::SwitchTabIndex(num) => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "switch_tab_index",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.switch_to_shortcut_number(num) {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::CloseTab(tab_id) => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "close_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                let outcome = tabs.close_tab(tab_id);
                                if outcome == CloseTabOutcome::LastTabClosed {
                                    *control_flow = ControlFlow::Exit;
                                } else if outcome == CloseTabOutcome::Closed {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::DuplicateTab(tab_id) => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "duplicate_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.duplicate_tab(tab_id, true).is_some() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::ReorderTab {
                                dragged_id,
                                target_id,
                            } => {
                                if tabs.reorder_tab(dragged_id, target_id) {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::SuspendTab(tab_id) => {
                                if tabs.suspend_tab(tab_id) {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::ReopenClosedTab => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "reopen_tab",
                                    &chrome_webview,
                                    &window,
                                );
                                if tabs.reopen_closed_tab().is_some() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::Navigate(raw_input) => {
                                storage.cancel_pending_queries();
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "navigation_start",
                                    &chrome_webview,
                                    &window,
                                );
                                let trimmed = raw_input.trim();
                                let is_direct_url = !trimmed.contains(' ')
                                    && (trimmed.contains('.')
                                        || trimmed.starts_with("http://")
                                        || trimmed.starts_with("https://")
                                        || trimmed.starts_with("localhost"));
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                let normalized_nav = crate::ui::normalize_omnibox_input(
                                    &raw_input,
                                    settings.get().search_engine,
                                );
                                let same_selftest_internal = selftest_active
                                    && pages::is_internal_url(&normalized_nav)
                                    && tabs
                                        .active_tab()
                                        .is_some_and(|t| t.url == normalized_nav);
                                if !same_selftest_internal
                                    && navigate_active_tab(
                                        &mut tabs,
                                        &mut env,
                                        &mut find_state,
                                        &raw_input,
                                    )
                                    && is_direct_url
                                    && !pages::is_internal_url(&normalized_nav)
                                {
                                    // Marked as typed (with the final, post-redirect URL) once
                                    // the load finishes; see `AppEvent::TabPageLoad`.
                                    let active_id = tabs.active_tab_id();
                                    tabs.set_pending_typed_url(active_id, Some(normalized_nav));
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                                if selftest_active {
                                    let _ = chrome_webview.focus();
                                }
                            }
                            UiCommand::Back => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "back",
                                    &chrome_webview,
                                    &window,
                                );
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ = webview.evaluate_script("window.history.back();");
                                    }
                                }
                            }
                            UiCommand::Forward => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "forward",
                                    &chrome_webview,
                                    &window,
                                );
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ =
                                            webview.evaluate_script("window.history.forward();");
                                    }
                                }
                            }
                            UiCommand::Reload => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "reload",
                                    &chrome_webview,
                                    &window,
                                );
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::Home => {
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "home",
                                    &chrome_webview,
                                    &window,
                                );
                                let home = settings.get().homepage.clone();
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                navigate_active_tab(&mut tabs, &mut env, &mut find_state, &home);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ToggleBookmark => {
                                if let Some(active) = tabs.active_tab() {
                                    storage.toggle_bookmark(&active.url, &active.title);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::ZoomIn => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_in);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ZoomOut => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_out);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ZoomReset => {
                                adjust_active_tab_zoom(&tabs, &storage, |_| zoom::zoom_reset());
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::FindQuery(q) => {
                                find_state.is_open = true;
                                find_state.query.clone_from(&q);
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let script = find::build_find_script(&q, true, true);
                                        let _ = webview.evaluate_script(&script);
                                    }
                                }
                            }
                            UiCommand::FindNext(q) => {
                                find_state.is_open = true;
                                find_state.query.clone_from(&q);
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let script = find::build_find_script(&q, true, false);
                                        let _ = webview.evaluate_script(&script);
                                    }
                                }
                            }
                            UiCommand::FindPrev(q) => {
                                find_state.is_open = true;
                                find_state.query.clone_from(&q);
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let script = find::build_find_script(&q, false, false);
                                        let _ = webview.evaluate_script(&script);
                                    }
                                }
                            }
                            UiCommand::FindClose => {
                                find_state.is_open = false;
                                find_state.active_match = 0;
                                find_state.total_matches = 0;
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ = webview
                                            .evaluate_script(find::build_clear_find_script());
                                        let _ = webview.focus();
                                    }
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::DownloadsClear => {
                                if let Ok(mut mgr) = downloads.lock() {
                                    mgr.clear_finished();
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ToggleAdblock => {
                                let new_state = adblocker.toggle();
                                settings.update(|s| s.adblock_enabled = new_state);
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ToggleSiteAllowlist => {
                                let active_url =
                                    tabs.active_tab().map(|t| t.url.clone()).unwrap_or_default();
                                if adblocker.toggle_site_allowlist(&active_url).is_some() {
                                    reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::WindowDrag => {
                                let _ = window.drag_window();
                            }
                            UiCommand::WindowResize(dir) => {
                                let _ = window.drag_resize_window(dir);
                            }
                            UiCommand::WindowMinimize => {
                                window.set_minimized(true);
                            }
                            UiCommand::WindowToggleMaximize => {
                                window.set_maximized(!window.is_maximized());
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::WindowClose => {
                                *control_flow = ControlFlow::Exit;
                            }
                            UiCommand::WindowSnap(zone) => {
                                ui::snap_window_to_zone(&window, zone);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                        }
                    }
                }
                AppEvent::PageIpc {
                    tab_id,
                    payload,
                    source_uri,
                } => {
                    let sender_url = tabs.get_tab(tab_id).map(|t| t.url.clone());
                    let parsed = ui::parse_page_ipc_event_checked(
                        &payload,
                        ui::internal_ipc_token(),
                        sender_url.as_deref(),
                        &source_uri,
                    )
                    .filter(|event| {
                        // Shortcuts / tab control / navigation only from the focused tab.
                        !ui::page_event_requires_active_sender(event)
                            || tab_id == tabs.active_tab_id()
                    });
                    if let Some(page_event) = parsed {
                        if overlay_controller.is_expanded()
                            && matches!(
                                page_event,
                                PageIpcEvent::ShortcutNewTab
                                    | PageIpcEvent::ShortcutCloseTab
                                    | PageIpcEvent::ShortcutReopenClosedTab
                                    | PageIpcEvent::ShortcutNextTab
                                    | PageIpcEvent::ShortcutPrevTab
                                    | PageIpcEvent::ShortcutSwitchTabIndex(_)
                                    | PageIpcEvent::ShortcutReload
                                    | PageIpcEvent::ShortcutBack
                                    | PageIpcEvent::ShortcutForward
                                    | PageIpcEvent::ShortcutFocusOmnibox
                            )
                        {
                            collapse_overlay_from_host(
                                &mut overlay_controller,
                                "page_shortcut",
                                &chrome_webview,
                                &window,
                            );
                        }
                        match page_event {
                            PageIpcEvent::FirstPaint => {
                                tabs.reveal_tab(tab_id, !overlay_controller.is_omnibox_focused());
                            }
                            PageIpcEvent::Favicon(fav_url) => {
                                if tabs.update_favicon(tab_id, Some(fav_url)) {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::Scroll(x, y) => {
                                tabs.update_scroll_position(tab_id, x, y);
                            }
                            // Main-frame only: iframe cosmetic messages arrive on the per-frame
                            // channel in `adblock::platform` and are applied to that frame.
                            PageIpcEvent::CosmeticInit(page_url)
                                if !adblock::cosmetic_message_matches_source(
                                    &page_url,
                                    &source_uri,
                                ) => {}
                            PageIpcEvent::CosmeticTokens { url, .. }
                                if !adblock::cosmetic_message_matches_source(&url, &source_uri) => {
                            }
                            PageIpcEvent::CosmeticInit(page_url) => {
                                let resources = adblocker.cosmetic_resources_for_url(&page_url);
                                let script = adblock::build_cosmetic_apply_script(&resources);
                                if let Some(tab) = tabs.get_tab(tab_id) {
                                    if let Some(webview) = &tab.webview {
                                        let _ = webview.evaluate_script(&script);
                                    }
                                }
                            }
                            PageIpcEvent::CosmeticTokens { url, classes, ids } => {
                                let resources = adblocker.cosmetic_resources_for_url(&url);
                                if !resources.generichide {
                                    let selectors = adblocker.hidden_class_id_selectors(
                                        &classes,
                                        &ids,
                                        &resources.exceptions,
                                    );
                                    for sel in &selectors {
                                        adblocker.record_debug_event(
                                            tab_id,
                                            adblock::AdblockMatchKind::CosmeticSelector,
                                            sel.clone(),
                                            url.clone(),
                                            String::from("generic-cosmetic"),
                                        );
                                    }
                                    let script =
                                        adblock::build_generic_selectors_script(&selectors);
                                    if !script.is_empty() {
                                        if let Some(tab) = tabs.get_tab(tab_id) {
                                            if let Some(webview) = &tab.webview {
                                                let _ = webview.evaluate_script(&script);
                                            }
                                        }
                                    }
                                }
                            }
                            PageIpcEvent::ScriptletLog { url, name, detail } => {
                                eprintln!(
                                    "[adblock-scriptlet] tab={} name={} detail={} url={}",
                                    tab_id, name, detail, url
                                );
                                let label = if detail.is_empty() {
                                    name
                                } else {
                                    format!("{name}({detail})")
                                };
                                adblocker.record_debug_event(
                                    tab_id,
                                    adblock::AdblockMatchKind::ScriptletExecuted,
                                    label,
                                    url,
                                    String::from("scriptlet"),
                                );
                            }
                            PageIpcEvent::FindResult { active, total } => {
                                if tab_id == tabs.active_tab_id() {
                                    find_state.active_match = active;
                                    find_state.total_matches = total;
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ContextOpenNewTab(target_url) => {
                                // Page-forgeable message: same popup rules as `window.open`.
                                let allowed = ui::is_safe_web_popup_target(&target_url)
                                    && tabs.try_consume_popup(tab_id, Instant::now());
                                if !allowed {
                                    eprintln!(
                                        "[ember][popup-blocker] blocked context-menu new tab from tab {tab_id}"
                                    );
                                } else if !adblocker.should_block(&target_url) {
                                    tabs.create_tab(&target_url, true);
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ContextCopyLink(href) => {
                                // Only reached for the active tab (see the sender filter above).
                                context_menu::copy_text_to_clipboard(&href);
                            }
                            PageIpcEvent::ContextInspect => {
                                #[cfg(debug_assertions)]
                                if let Some(tab) = tabs.get_tab(tab_id) {
                                    if let Some(webview) = &tab.webview {
                                        webview.open_devtools();
                                    }
                                }
                            }
                            PageIpcEvent::InternalNavigate(raw_target) => {
                                let Some(safe_target) = ui::resolve_internal_navigation(
                                    &raw_target,
                                    settings.get().search_engine,
                                ) else {
                                    return;
                                };
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                navigate_active_tab(
                                    &mut tabs,
                                    &mut env,
                                    &mut find_state,
                                    &safe_target,
                                );
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::HistoryClear => {
                                storage.clear_history();
                                reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                            }
                            PageIpcEvent::HistoryDelete(del_url) => {
                                storage.delete_history_url(&del_url);
                                reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                            }
                            PageIpcEvent::BookmarkRemove(rem_url) => {
                                storage.remove_bookmark(&rem_url);
                                reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutNewTab => {
                                let s = settings.get();
                                open_input_in_new_tab(&mut tabs, &s.homepage, s.search_engine, true);
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutCloseTab => {
                                let outcome = tabs.close_tab(tab_id);
                                if outcome == CloseTabOutcome::LastTabClosed {
                                    *control_flow = ControlFlow::Exit;
                                } else if outcome == CloseTabOutcome::Closed {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutReopenClosedTab => {
                                if tabs.reopen_closed_tab().is_some() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutNextTab => {
                                if tabs.switch_next_tab() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutPrevTab => {
                                if tabs.switch_prev_tab() {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutSwitchTabIndex(num) => {
                                if tabs.switch_to_shortcut_number(num) {
                                    let mut env = WebViewEnv {
                                        window: &window,
                                        web_context: &mut web_context,
                                        storage: &storage,
                                        adblocker: &adblocker,
                                        permissions: &permissions,
                                        settings: &settings,
                                        downloads: &downloads,
                                        proxy: &proxy,
                                        content_bounds: current_content_bounds,
                                    };
                                    let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutReload => {
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutBack => {
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ = webview.evaluate_script("window.history.back();");
                                    }
                                }
                            }
                            PageIpcEvent::ShortcutForward => {
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ =
                                            webview.evaluate_script("window.history.forward();");
                                    }
                                }
                            }
                            PageIpcEvent::ShortcutFocusOmnibox => {
                                let _ = chrome_webview.focus();
                                let _ = chrome_webview.evaluate_script(
                                    "if(window.__emberFocusOmnibox){window.__emberFocusOmnibox();}",
                                );
                            }
                            PageIpcEvent::ShortcutToggleBookmark => {
                                if let Some(active) = tabs.active_tab() {
                                    storage.toggle_bookmark(&active.url, &active.title);
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ShortcutOpenFind => {
                                find_state.is_open = true;
                                let _ = chrome_webview.focus();
                                let _ = chrome_webview.evaluate_script(
                                    "if(window.__emberOpenFind){window.__emberOpenFind();}",
                                );
                            }
                            PageIpcEvent::ShortcutZoomIn => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_in);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutZoomOut => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_out);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutZoomReset => {
                                adjust_active_tab_zoom(&tabs, &storage, |_| zoom::zoom_reset());
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::UserActivation => {
                                tabs.note_user_activation(tab_id, Instant::now());
                            }
                            PageIpcEvent::PointerDown => {
                                tabs.note_user_activation(tab_id, Instant::now());
                                collapse_overlay_from_host(
                                    &mut overlay_controller,
                                    "page_pointer_down",
                                    &chrome_webview,
                                    &window,
                                );
                            }
                            PageIpcEvent::SpaNav(spa_url) => {
                                let old_url = tabs.get_tab(tab_id).map(|t| t.url.clone());
                                let changed = tabs.update_page_load(tab_id, spa_url.clone(), false);
                                if let Some(tab) = tabs.get_tab(tab_id) {
                                    // Only real URL changes (not replaceState / hash-only
                                    // updates of the same document URL) create a visit.
                                    if old_url.as_deref().is_some_and(|old| {
                                        crate::tabs::should_record_spa_visit(old, &tab.url)
                                    }) {
                                        storage.record_visit_async(&tab.url, &tab.title);
                                    }
                                }
                                if changed {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &mut find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::SettingsSet { key, value } => {
                                let mut theme_to_apply = None;
                                let mut suspend_changed = false;
                                let mut reload_needed = false;

                                let updated = settings.update(|s| match key.as_str() {
                                    "search_engine" => {
                                        s.search_engine = SearchEngine::parse(&value);
                                    }
                                    "homepage" => {
                                        s.homepage = ui::normalize_homepage(&value, s.search_engine);
                                    }
                                    "theme" => {
                                        let theme = ThemePreference::parse(&value);
                                        s.theme = theme;
                                        theme_to_apply = Some(theme);
                                    }
                                    "tab_suspend_minutes" => {
                                        if let Ok(mins) = value.parse::<u64>() {
                                            // Same clamp as the TOML parser; 0 means "Never".
                                            s.tab_suspend_minutes =
                                                crate::settings::clamp_suspend_minutes(mins);
                                            suspend_changed = true;
                                        }
                                    }
                                    "adblock_enabled" => {
                                        let en = value == "true";
                                        s.adblock_enabled = en;
                                        adblocker.set_enabled(en);
                                    }
                                    "easylist_enabled" => {
                                        let en = value == "true";
                                        s.easylist_enabled = en;
                                        adblocker.set_filter_lists(
                                            s.easylist_enabled,
                                            s.easyprivacy_enabled,
                                        );
                                        reload_needed = true;
                                    }
                                    "easyprivacy_enabled" => {
                                        let en = value == "true";
                                        s.easyprivacy_enabled = en;
                                        adblocker.set_filter_lists(
                                            s.easylist_enabled,
                                            s.easyprivacy_enabled,
                                        );
                                        reload_needed = true;
                                    }
                                    "block_third_party_cookies" => {
                                        let en = value == "true";
                                        s.block_third_party_cookies = en;
                                        adblocker.set_privacy_flags(s.send_dnt_and_gpc, en);
                                    }
                                    "send_dnt_and_gpc" => {
                                        let en = value == "true";
                                        s.send_dnt_and_gpc = en;
                                        adblocker
                                            .set_privacy_flags(en, s.block_third_party_cookies);
                                    }
                                    "https_only_mode" => {
                                        s.https_only_mode = value == "true";
                                    }
                                    "search_suggestions_enabled" => {
                                        s.search_suggestions_enabled = value == "true";
                                    }
                                    "tab_animations_enabled" => {
                                        s.tab_animations_enabled = value == "true";
                                    }
                                    _ => {}
                                });

                                if let Some(theme) = theme_to_apply {
                                    ui::apply_theme_preference(&window, theme);
                                    ui::apply_webview_theme(&chrome_webview, theme);
                                    let content_bg = ui::theme_background_rgba(theme, &window);
                                    tabs.for_each_webview(|wv| {
                                        ui::apply_webview_theme(wv, theme);
                                        let _ = wv.set_background_color(content_bg);
                                    });
                                }
                                if suspend_changed {
                                    // `tab_suspend_timeout()` maps 0 ("Never") to 365 days.
                                    tabs.set_suspend_timeout(updated.tab_suspend_timeout());
                                }
                                if reload_needed {
                                    reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ThemePreview(payload) => {
                                crate::theme::set_current(crate::theme::parse_payload(&payload));
                                apply_custom_theme_to_chrome(&chrome_webview);
                                apply_custom_theme_to_tab(&tabs, tab_id);
                            }
                            PageIpcEvent::ThemeSave(payload) => {
                                let theme = crate::theme::parse_payload(&payload);
                                if let Err(err) = crate::theme::save(theme) {
                                    eprintln!("[ember][theme] failed to save theme: {err}");
                                }
                                apply_custom_theme_to_chrome(&chrome_webview);
                                apply_custom_theme_to_tab(&tabs, tab_id);
                            }
                            PageIpcEvent::ThemeRevert => {
                                crate::theme::reload_saved();
                                apply_custom_theme_to_chrome(&chrome_webview);
                                apply_custom_theme_to_tab(&tabs, tab_id);
                            }
                            PageIpcEvent::SettingsAction { name, arg } => {
                                match name.as_str() {
                                    "update_filters" => {
                                        adblocker.update_now();
                                        let blocker_clone = adblocker.clone();
                                        let proxy_clone = proxy.clone();
                                        let _ = thread::Builder::new()
                                            .name(String::from("ember-settings-update-poll"))
                                            .spawn(move || {
                                                thread::sleep(Duration::from_millis(150));
                                                while blocker_clone.is_updating() {
                                                    thread::sleep(Duration::from_millis(200));
                                                }
                                                let _ = proxy_clone
                                                    .send_event(AppEvent::RefreshSettingsPage);
                                            });
                                        if tabs
                                            .get_tab(tab_id)
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                        }
                                    }
                                    "clear_data" => {
                                        let clear_history = arg.contains("history");
                                        let clear_cookies = arg.contains("cookies");
                                        let clear_cache = arg.contains("cache");
                                        if clear_history {
                                            storage.clear_history();
                                        }
                                        if clear_cookies || clear_cache {
                                            if let Some(active) = tabs.active_tab() {
                                                if let Some(wv) = &active.webview {
                                                    privacy::clear_browsing_data(
                                                        wv,
                                                        clear_cookies,
                                                        clear_cache,
                                                    );
                                                }
                                            } else {
                                                privacy::clear_browsing_data(
                                                    &chrome_webview,
                                                    clear_cookies,
                                                    clear_cache,
                                                );
                                            }
                                        }
                                        if tabs
                                            .get_tab(tab_id)
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                        }
                                    }
                                    "allowlist_add" => {
                                        let host = arg.trim();
                                        if !host.is_empty() {
                                            adblocker.set_site_allowlisted(host, true);
                                            if tabs
                                            .get_tab(tab_id)
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                            }
                                        }
                                    }
                                    "allowlist_remove" => {
                                        let host = arg.trim();
                                        if !host.is_empty() {
                                            adblocker.set_site_allowlisted(host, false);
                                            if tabs
                                            .get_tab(tab_id)
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_tab(&mut tabs, tab_id, &storage, &settings, &adblocker);
                                            }
                                        }
                                    }
                                    "open" => {
                                        let Some(safe_target) = ui::resolve_internal_navigation(
                                            &arg,
                                            settings.get().search_engine,
                                        ) else {
                                            return;
                                        };
                                        let mut env = WebViewEnv {
                                            window: &window,
                                            web_context: &mut web_context,
                                            storage: &storage,
                                            adblocker: &adblocker,
                                            permissions: &permissions,
                                            settings: &settings,
                                            downloads: &downloads,
                                            proxy: &proxy,
                                            content_bounds: current_content_bounds,
                                        };
                                        navigate_active_tab(
                                            &mut tabs,
                                            &mut env,
                                            &mut find_state,
                                            &safe_target,
                                        );
                                    }
                                    "run_selftest" => {
                                        selftest_active = true;
                                        selftest_saw_popup_open = false;
                                        selftest_saw_popup_close = false;
                                        selftest_bounds_always_restored = true;
                                        selftest_overlay_open_count = 0;
                                        selftest_overlay_close_count = 0;
                                        storage.record_visit(
                                            "https://selftest.ember.local/guide",
                                            "Ember Self-Test Guide",
                                        );
                                        window.set_focus();
                                        let _ = chrome_webview.focus();
                                        let _ = chrome_webview.evaluate_script(
                                            "if (typeof window.__emberRunSelfTest === 'function') { window.__emberRunSelfTest(); }",
                                        );
                                    }
                                    _ => {}
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::HttpsProceed(target) => {
                                settings.allow_http_for_url(&target);
                                let mut env = WebViewEnv {
                                    window: &window,
                                    web_context: &mut web_context,
                                    storage: &storage,
                                    adblocker: &adblocker,
                                    permissions: &permissions,
                                    settings: &settings,
                                    downloads: &downloads,
                                    proxy: &proxy,
                                    content_bounds: current_content_bounds,
                                };
                                navigate_active_tab(&mut tabs, &mut env, &mut find_state, &target);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &mut find_state,
                                    &downloads,
                                );
                            }
                        }
                    }
                }
                AppEvent::TabTitleChanged { tab_id, title } => {
                    if let Some(tab) = tabs.get_tab(tab_id) {
                        if !tab.showing_blocked_notice {
                            storage.update_visit_title_async(&tab.url, &title);
                        }
                    }
                    if tabs.update_title(tab_id, title) {
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &mut find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::TabPageLoad {
                    tab_id,
                    url,
                    is_loading,
                } => {
                    if is_loading && tab_id == tabs.active_tab_id() {
                        collapse_overlay_from_host(
                            &mut overlay_controller,
                            "navigation_start",
                            &chrome_webview,
                            &window,
                        );
                    }
                    let changed = tabs.update_page_load(tab_id, url, is_loading);
                    if !is_loading {
                        tabs.reveal_tab(tab_id, !overlay_controller.is_omnibox_focused());
                        let typed_final_url = tabs.take_typed_url_on_load_finished(tab_id);
                        if let Some(tab) = tabs.get_tab(tab_id).filter(|t| !t.showing_blocked_notice) {
                            storage.record_visit_async(&tab.url, &tab.title);
                            // Mark the *final* (post-redirect) URL as typed, after its visit row
                            // exists, exactly once per omnibox navigation.
                            if let Some(typed) = &typed_final_url {
                                storage.mark_url_typed_async(typed);
                            }
                            let factor = resolve_zoom_for_url(&storage, &tab.url);
                            if let Some(webview) = &tab.webview {
                                let _ = webview.zoom(factor);
                            }
                        }
                        if tab_id == tabs.active_tab_id() {
                            if let Some(runner) = &mut bench_runner {
                                if runner.step_finished_load_at.is_none() {
                                    runner.step_finished_load_at = Some(Instant::now());
                                }
                            }
                        }
                        if let Some(ref report_json) = last_selftest_report_json {
                            if let Some(tab) = tabs.get_tab(tab_id) {
                                if tab.url == pages::SELFTEST_URL {
                                    if let Some(wv) = &tab.webview {
                                        let _ = wv.evaluate_script(&format!(
                                            "if (typeof window.__emberRenderSelfTestReport === 'function') {{ window.__emberRenderSelfTestReport({report_json}); }}"
                                        ));
                                    }
                                }
                            }
                        }
                        if let Some((scroll_x, scroll_y)) = tabs.take_pending_scroll_restore(tab_id)
                        {
                            if (scroll_x, scroll_y) != (0.0, 0.0) {
                                if let Some(tab) = tabs.get_tab(tab_id) {
                                    if let Some(webview) = &tab.webview {
                                        let script =
                                            format!("window.scrollTo({scroll_x}, {scroll_y});");
                                        let _ = webview.evaluate_script(&script);
                                    }
                                }
                            }
                        }
                    }
                    if changed {
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &mut find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::AdBlocked { tab_id } => {
                    if tabs.increment_blocked_count(tab_id) && tab_id == tabs.active_tab_id() {
                        let count = tabs.active_tab().map_or(0, |t| t.blocked_count);
                        let script = format!(
                            "if(window.__emberUpdateBlockedBadge){{window.__emberUpdateBlockedBadge({count});}}"
                        );
                        let _ = chrome_webview.evaluate_script(&script);
                    }
                }
                AppEvent::OpenInNewTab { tab_id, url } => {
                    // Popup blocker: require a recent, unused user activation in the source tab.
                    let allowed = ui::is_safe_web_popup_target(&url)
                        && tabs.try_consume_popup(tab_id, Instant::now());
                    if !allowed {
                        eprintln!("[ember][popup-blocker] blocked new window from tab {tab_id}");
                    } else if !adblocker.should_block(&url) {
                        tabs.create_tab(&url, true);
                        let mut env = WebViewEnv {
                            window: &window,
                            web_context: &mut web_context,
                            storage: &storage,
                            adblocker: &adblocker,
                            permissions: &permissions,
                            settings: &settings,
                            downloads: &downloads,
                            proxy: &proxy,
                            content_bounds: current_content_bounds,
                        };
                        let _ = ensure_active_tab_webview(&mut tabs, &mut env);
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &mut find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::HttpsBlocked { tab_id, url } => {
                    let warning_url = pages::warning_url_for(&url);
                    if tab_id == tabs.active_tab_id() {
                        let mut env = WebViewEnv {
                            window: &window,
                            web_context: &mut web_context,
                            storage: &storage,
                            adblocker: &adblocker,
                            permissions: &permissions,
                            settings: &settings,
                            downloads: &downloads,
                            proxy: &proxy,
                            content_bounds: current_content_bounds,
                        };
                        navigate_active_tab(&mut tabs, &mut env, &mut find_state, &warning_url);
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &mut find_state,
                            &downloads,
                        );
                    } else if let Some(tab) = tabs.get_tab_mut(tab_id) {
                        tab.url = warning_url.clone();
                        tab.title = String::from("Connection not secure");
                        if let Some(wv) = &tab.webview {
                            let page_ctx = PageCtx {
                                storage: &storage,
                                settings: &settings,
                                adblocker: &adblocker,
                            };
                            load_target_into_webview(wv, &warning_url, &page_ctx);
                        }
                    }
                }
                AppEvent::RefreshSettingsPage => {
                    if tabs
                        .active_tab()
                        .is_some_and(|t| t.url == pages::SETTINGS_URL)
                    {
                        reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &mut find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::TabRevealTimeout { tab_id } => {
                    tabs.reveal_tab(tab_id, !overlay_controller.is_omnibox_focused());
                }
                AppEvent::OmniboxSuggestions {
                    query_id,
                    autofill,
                    suggestions,
                } => {
                    if !suggestions.is_empty() && overlay_controller.is_omnibox_focused() {
                        let already_omnibox = overlay_controller.is_expanded()
                            && overlay_controller.active_kind()
                                == Some(crate::popup::OverlayKind::Omnibox);
                        overlay_controller.expand(
                            crate::popup::OverlayKind::Omnibox,
                            &chrome_webview,
                            &window,
                        );
                        selftest_saw_popup_open = true;
                        if !already_omnibox {
                            selftest_overlay_open_count += 1;
                        }
                    }
                    let mut items_json = String::from("[");
                    for (idx, item) in suggestions.iter().enumerate() {
                        if idx > 0 {
                            items_json.push(',');
                        }
                        items_json.push_str("{\"kind\":");
                        push_js_quoted(&mut items_json, item.kind.as_str());
                        items_json.push_str(",\"title\":");
                        push_js_quoted(&mut items_json, &item.title);
                        items_json.push_str(",\"url\":");
                        push_js_quoted(&mut items_json, &item.url);
                        items_json.push_str(",\"isDefault\":");
                        items_json.push_str(if item.is_default { "true" } else { "false" });
                        items_json.push('}');
                    }
                    items_json.push(']');

                    let autofill_json = match autofill {
                        Some(ref af) => {
                            let mut s = String::from("{\"completedText\":");
                            push_js_quoted(&mut s, &af.completed_text);
                            s.push_str(",\"fullUrl\":");
                            push_js_quoted(&mut s, &af.full_url);
                            s.push_str(",\"title\":");
                            push_js_quoted(&mut s, &af.title);
                            s.push_str(",\"kind\":");
                            push_js_quoted(&mut s, af.kind.as_str());
                            s.push('}');
                            s
                        }
                        None => String::from("null"),
                    };

                    let script = format!(
                        "if(window.__emberShowOmniboxSuggestions){{window.__emberShowOmniboxSuggestions({query_id},{items_json},{autofill_json});}}"
                    );
                    let _ = chrome_webview.evaluate_script(&script);
                }
                AppEvent::UiReadyTimeout => {
                    if cfg!(debug_assertions) && (!ui_ready_received || !ui_failed_modules.is_empty())
                    {
                        if ui_failed_modules.is_empty() {
                            let msg = "ERROR: 'ui_ready' IPC did not arrive within 2s; failed modules: [popup_client, toolbar, omnibox, tab_strip]";
                            eprintln!("[ember][ui-self-check] {msg}");
                            storage.log_ui_js_error("ui_ready_timeout", msg);
                        } else {
                            let list: Vec<String> = ui_failed_modules
                                .iter()
                                .map(|(m, e)| format!("{m} ({e})"))
                                .collect();
                            let msg = format!(
                                "ERROR: UI initialization incomplete after 2s; failed modules: {}",
                                list.join(", ")
                            );
                            eprintln!("[ember][ui-self-check] {msg}");
                            storage.log_ui_js_error("ui_ready_timeout", &msg);
                        }
                    }
                }
                AppEvent::SelfTestTimeout => {
                    if selftest_mode && selftest_active {
                        eprintln!("FAIL: self-test timed out after 25s");
                        storage.log_ui_js_error("selftest", "Self-test timed out after 25s");
                        if let Some(ref dir) = selftest_temp_dir {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        std::process::exit(1);
                    }
                }
            },
            _ => {}
        }
    });
}

fn collapse_overlay_from_host(
    overlay: &mut crate::popup::OverlayController,
    reason: &str,
    chrome_webview: &WebView,
    window: &Window,
) {
    if overlay.is_expanded() {
        overlay.collapse(reason, chrome_webview, window);
        let _ = chrome_webview
            .evaluate_script("if(window.closeSuggestions){window.closeSuggestions('host_event');}");
    }
}

/// Pushes the current custom theme CSS into the chrome webview (`window.__emberSetCustomTheme`).
fn apply_custom_theme_to_chrome(chrome_webview: &WebView) {
    let css = crate::theme::current().chrome_css();
    let mut script = String::from(
        "if(typeof window.__emberSetCustomTheme==='function'){window.__emberSetCustomTheme(",
    );
    push_js_quoted(&mut script, &css);
    script.push_str(");}");
    let _ = chrome_webview.evaluate_script(&script);
}

/// Pushes the current custom theme's page CSS into an internal page tab (the theme editor).
fn apply_custom_theme_to_tab(tabs: &TabManager, tab_id: usize) {
    let Some(webview) = tabs.get_tab(tab_id).and_then(|t| t.webview.as_ref()) else {
        return;
    };
    let css = crate::theme::current().page_css();
    let mut script = String::from(
        "if(typeof window.__emberSetCustomTheme==='function'){window.__emberSetCustomTheme(",
    );
    push_js_quoted(&mut script, &css);
    script.push_str(");}");
    let _ = webview.evaluate_script(&script);
}

fn push_js_quoted(out: &mut String, s: &str) {
    out.push('"');
    ui::push_js_escaped(out, s);
    out.push('"');
}

fn open_directory_in_shell(dir: &std::path::Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelfTestItem {
    name: String,
    pass: bool,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelfTestParsedReport {
    items: Vec<SelfTestItem>,
    dom_count: Option<usize>,
    all_dom_count: Option<usize>,
}

fn parse_selftest_report(json: &str) -> SelfTestParsedReport {
    let mut items = Vec::new();
    if let Some(items_pos) = json.find("\"items\"") {
        let rest = &json[items_pos + "\"items\"".len()..];
        if let Some(arr_start) = rest.find('[') {
            let arr_slice = &rest[arr_start + 1..];
            let mut in_str = false;
            let mut escape = false;
            let mut obj_depth = 0i32;
            let mut obj_start = None;

            for (idx, ch) in arr_slice.char_indices() {
                if escape {
                    escape = false;
                    continue;
                }
                if in_str {
                    if ch == '\\' {
                        escape = true;
                    } else if ch == '"' {
                        in_str = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_str = true,
                    '{' => {
                        if obj_depth == 0 {
                            obj_start = Some(idx);
                        }
                        obj_depth += 1;
                    }
                    '}' => {
                        obj_depth -= 1;
                        if obj_depth == 0 {
                            if let Some(start) = obj_start.take() {
                                let obj = &arr_slice[start..=idx];
                                let name = crate::popup::extract_json_string(obj, "name")
                                    .unwrap_or_else(|| String::from("unknown"));
                                let pass =
                                    crate::popup::extract_json_bool(obj, "pass").unwrap_or(false);
                                let detail = crate::popup::extract_json_string(obj, "detail")
                                    .unwrap_or_default();
                                items.push(SelfTestItem { name, pass, detail });
                            }
                        }
                    }
                    ']' if obj_depth == 0 => break,
                    _ => {}
                }
            }
        }
    }

    let dom_count = crate::popup::extract_json_number(json, "domCount").map(|n| n as usize);
    let all_dom_count = crate::popup::extract_json_number(json, "allDomCount").map(|n| n as usize);
    SelfTestParsedReport {
        items,
        dom_count,
        all_dom_count,
    }
}

fn build_selftest_report_json(items: &[SelfTestItem]) -> String {
    let mut out = String::from("{\"items\":[");
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        push_js_quoted(&mut out, &item.name);
        out.push_str(",\"pass\":");
        out.push_str(if item.pass { "true" } else { "false" });
        out.push_str(",\"detail\":");
        push_js_quoted(&mut out, &item.detail);
        out.push('}');
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_selftest_report_extracts_items_and_counts() {
        let json = r#"{"items":[{"name":"UI scripts initialized without errors","pass":true,"detail":"ok"},{"name":"DOM tab count equals Rust tab count","pass":false,"detail":"mismatch {0}"}],"domCount":2,"allDomCount":2}"#;
        let parsed = parse_selftest_report(json);
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(
            parsed.items[0].name,
            "UI scripts initialized without errors"
        );
        assert!(parsed.items[0].pass);
        assert_eq!(parsed.items[1].detail, "mismatch {0}");
        assert!(!parsed.items[1].pass);
        assert_eq!(parsed.dom_count, Some(2));
        assert_eq!(parsed.all_dom_count, Some(2));

        let rebuilt = build_selftest_report_json(&parsed.items);
        assert!(rebuilt.contains("\"pass\":true"));
        assert!(rebuilt.contains("\"pass\":false"));
    }

    #[test]
    fn test_find_state_resets_on_tab_switch() {
        let mut fs = FindState {
            is_open: true,
            query: String::from("needle"),
            active_match: 2,
            total_matches: 7,
        };
        // Same tab: untouched.
        assert!(!reset_find_state_for_tab_change(&mut fs, 3, 3));
        assert!(fs.is_open);
        assert_eq!(fs.total_matches, 7);
        // Different tab: closed and counts cleared; caller must clear old highlights.
        assert!(reset_find_state_for_tab_change(&mut fs, 3, 4));
        assert!(!fs.is_open);
        assert_eq!((fs.active_match, fs.total_matches), (0, 0));
        // Nothing open: switching needs no highlight clearing.
        assert!(!reset_find_state_for_tab_change(&mut fs, 4, 5));
    }

    #[test]
    fn test_blocked_notice_html_escapes_url() {
        let html = blocked_notice_html("https://ads.example/<script>alert('x')</script>&a=\"1\"");
        assert!(html.contains("<title>Blocked</title>"));
        assert!(!html.contains("<script>"));
        assert!(
            html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;a=&quot;1&quot;")
        );
    }

    #[test]
    fn test_open_input_in_new_tab_normalizes() {
        let mut tabs = TabManager::new(Duration::from_secs(60));
        let id = open_input_in_new_tab(&mut tabs, "example.com", SearchEngine::DuckDuckGo, true);
        assert_eq!(
            tabs.get_tab(id).map(|t| t.url.as_str()),
            Some("https://example.com")
        );
        let settings_id = open_input_in_new_tab(
            &mut tabs,
            "ember://settings",
            SearchEngine::DuckDuckGo,
            true,
        );
        let again = open_input_in_new_tab(
            &mut tabs,
            "EMBER://settings/",
            SearchEngine::DuckDuckGo,
            true,
        );
        assert_eq!(settings_id, again);
        let bg = open_input_in_new_tab(&mut tabs, "", SearchEngine::DuckDuckGo, false);
        assert_eq!(
            tabs.get_tab(bg).map(|t| t.url.as_str()),
            Some("ember://newtab")
        );
        assert_eq!(tabs.active_tab_id(), settings_id);
    }
}
