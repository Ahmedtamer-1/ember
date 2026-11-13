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
    /// Raw IPC message emitted inside the popup overlay WebView.
    PopupIpc(String),
    /// Raw IPC message emitted inside a tab's content WebView.
    PageIpc { tab_id: usize, payload: String },
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
    /// A page requested opening a URL in a new window/tab (`target="_blank"` or `window.open`).
    OpenInNewTab(String),
    /// A file download started or completed.
    DownloadUpdated,
    /// HTTPS-only mode intercepted a navigation to an insecure `http://` URL inside `tab_id`.
    HttpsBlocked { tab_id: usize, url: String },
    /// A background task changed state that `ember://settings` displays (e.g. filter update done).
    RefreshSettingsPage,
    /// Ranked omnibox suggestions computed on the background DB worker thread.
    OmniboxSuggestions {
        query_id: u64,
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
        .with_visible(true)
        .with_clipboard(true)
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
            let _ = popup_proxy.send_event(AppEvent::OpenInNewTab(target_url));
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

    // Apply remembered per-site zoom
    let zoom_factor = resolve_zoom_for_url(env.storage, &url);
    let _ = webview.zoom(zoom_factor);

    // Apply active theme to the new webview
    ui::apply_webview_theme(&webview, current_settings.theme);

    // Load either internal `ember://` HTML or external URL
    let page_ctx = env.page_ctx();
    load_target_into_webview(&webview, &url, &page_ctx);

    tabs.attach_webview(tab_id, webview);
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

fn navigate_active_tab(
    tabs: &mut TabManager,
    env: &mut WebViewEnv<'_>,
    find_state: &mut FindState,
    raw_input: &str,
) {
    let mut target_url = ui::normalize_omnibox_input(raw_input, env.settings.get().search_engine);
    if let Some(canonical) = pages::is_singleton_internal_url(&target_url) {
        tabs.open_or_focus_internal_page(canonical);
        let _ = ensure_active_tab_webview(tabs, env);
        return;
    }
    if env.settings.should_intercept_http(&target_url) {
        target_url = pages::warning_url_for(&target_url);
    }
    if !pages::is_internal_url(&target_url) && env.adblocker.should_block(&target_url) {
        return;
    }

    find_state.active_match = 0;
    find_state.total_matches = 0;

    let _ = ensure_active_tab_webview(tabs, env);
    let active_id = tabs.active_tab_id();
    tabs.reset_blocked_count(active_id);

    let zoom_factor = resolve_zoom_for_url(env.storage, &target_url);
    let page_ctx = env.page_ctx();

    if let Some(active) = tabs.active_tab_mut() {
        active.url.clone_from(&target_url);
        active.favicon_url = derive_default_favicon(&target_url);
        active.scroll_position = (0.0, 0.0);

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
                let _ = webview.zoom(zoom_factor);
                let _ = webview.load_url(&target_url);
                let _ = webview.focus();
            }
        }
    }
}

fn reload_active_tab(
    tabs: &mut TabManager,
    storage: &Storage,
    settings: &SettingsStore,
    adblocker: &Adblocker,
) {
    if let Some(active) = tabs.active_tab_mut() {
        active.blocked_count = 0;
        let url = active.url.clone();
        let page_ctx = PageCtx {
            storage,
            settings,
            adblocker,
        };
        if let Some((title, html)) = pages::render_internal_page(&url, &page_ctx) {
            active.title = title;
            active.is_loading = false;
            if let Some(webview) = &active.webview {
                let _ = webview.load_html(&html);
            }
        } else if let Some(webview) = &active.webview {
            let _ = webview.reload();
        }
    }
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
    find_state: &FindState,
    downloads: &Arc<Mutex<DownloadManager>>,
) {
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

    let sync_state = ChromeSyncState {
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

    let storage = Storage::init()?;
    let settings = SettingsStore::init()?;
    let init_settings = settings.get();
    let adblocker = Adblocker::with_filters_dir(
        storage.filters_dir(),
        init_settings.easylist_enabled,
        init_settings.easyprivacy_enabled,
    );
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
    ui::apply_theme_preference(&window, init_settings.theme);

    let (initial_chrome_bounds, mut current_content_bounds) =
        ui::compute_layout_bounds(window.inner_size(), window.scale_factor());

    let mut web_context = WebContext::new(Some(storage.webview_data_dir().to_path_buf()));

    let ipc_proxy = proxy.clone();
    let chrome_webview = WebViewBuilder::with_web_context(&mut web_context)
        .with_bounds(initial_chrome_bounds)
        .with_html(ui::CHROME_HTML)
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(AppEvent::UiIpc(request.body().clone()));
        })
        .build_as_child(&window)?;
    ui::apply_webview_theme(&chrome_webview, init_settings.theme);

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

    tabs.create_tab(&initial_url, true);

    if bench_runner.is_none() && cli_urls.len() > 1 {
        for url in &cli_urls[1..] {
            tabs.create_tab(url, false);
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
        &find_state,
        &downloads,
    );

    let mut popup_manager = crate::popup::PopupManager::new();

    let check_interval = tabs
        .suspend_timeout()
        .min(SUSPEND_CHECK_INTERVAL)
        .max(Duration::from_secs(1));

    event_loop.run(move |event, window_target, control_flow| {
        let wait_slice = if bench_runner.is_some() {
            Duration::from_millis(250)
        } else {
            check_interval
        };
        *control_flow = ControlFlow::WaitUntil(Instant::now() + wait_slice);

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
                        &find_state,
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
                &find_state,
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
                if popup_manager.is_open() {
                    popup_manager.hide_now();
                    let _ = chrome_webview.evaluate_script(
                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                    );
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Focused(false),
                ..
            } => {
                if popup_manager.is_open()
                    && (popup_manager.is_popup_window(window_id)
                        || !popup_manager.is_foreground_ours(&window))
                {
                    popup_manager.close(true);
                    let _ = chrome_webview.evaluate_script(
                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                    );
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::Resized(physical_size),
                ..
            } if window_id == window.id() => {
                if popup_manager.is_open() {
                    popup_manager.hide_now();
                    let _ = chrome_webview.evaluate_script(
                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                    );
                }
                let (chrome_bounds, content_bounds) =
                    ui::compute_layout_bounds(physical_size, window.scale_factor());
                current_content_bounds = content_bounds;
                let _ = chrome_webview.set_bounds(chrome_bounds);
                tabs.resize_all(content_bounds);
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &find_state,
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
                if popup_manager.is_open() {
                    popup_manager.hide_now();
                    let _ = chrome_webview.evaluate_script(
                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                    );
                }
                let (chrome_bounds, content_bounds) =
                    ui::compute_layout_bounds(*new_inner_size, scale_factor);
                current_content_bounds = content_bounds;
                let _ = chrome_webview.set_bounds(chrome_bounds);
                tabs.resize_all(content_bounds);
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &find_state,
                    &downloads,
                );
            }
            Event::WindowEvent {
                event: WindowEvent::ThemeChanged(_),
                ..
            } => {
                ui::apply_window_chrome_style(&window);
                sync_chrome_ui(
                    &chrome_webview,
                    &window,
                    &tabs,
                    &storage,
                    &adblocker,
                    &settings,
                    &find_state,
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
                        &find_state,
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::NewTab => {
                                if popup_manager.is_open() {
                                    popup_manager.hide_now();
                                    let _ = chrome_webview.evaluate_script(
                                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                                    );
                                }
                                tabs.create_tab(&settings.get().homepage, true);
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::NewWindow => {
                                if let Ok(exe) = std::env::current_exe() {
                                    let _ = std::process::Command::new(exe).spawn();
                                }
                            }
                            UiCommand::OpenNewTab(target_url) => {
                                if popup_manager.is_open() {
                                    popup_manager.hide_now();
                                    let _ = chrome_webview.evaluate_script(
                                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                                    );
                                }
                                let normalized = ui::normalize_omnibox_input(
                                    &target_url,
                                    settings.get().search_engine,
                                );
                                if let Some(canonical) = pages::is_singleton_internal_url(&normalized)
                                {
                                    tabs.open_or_focus_internal_page(canonical);
                                } else {
                                    tabs.create_tab(&normalized, true);
                                }
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
                                    &find_state,
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
                            UiCommand::OpenPopup(json_payload) => {
                                if let Some(req) = crate::popup::parse_open_popup_payload(&json_payload) {
                                    let mut appearance = ui::query_window_appearance(&window);
                                    appearance.is_dark =
                                        settings.get().theme.resolve_is_dark(appearance.is_dark);
                                    let reduced_motion = !settings.get().tab_animations_enabled;
                                    let _ = popup_manager.open(
                                        crate::popup::PopupOpenContext {
                                            event_loop: window_target,
                                            main_window: &window,
                                            web_context: &mut web_context,
                                            proxy: &proxy,
                                            appearance: &appearance,
                                            reduced_motion,
                                        },
                                        &req,
                                    );
                                }
                            }
                            UiCommand::ClosePopup => {
                                popup_manager.close(true);
                                let _ = chrome_webview.evaluate_script(
                                    "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                                );
                            }
                            UiCommand::OmniboxSuggest { query_id, query } => {
                                let s = settings.get();
                                let suggest_proxy = proxy.clone();
                                storage.suggest_async(
                                    query_id,
                                    query,
                                    8,
                                    s.search_suggestions_enabled,
                                    s.search_engine,
                                    move |qid, suggestions| {
                                        let _ = suggest_proxy.send_event(
                                            AppEvent::OmniboxSuggestions {
                                                query_id: qid,
                                                suggestions,
                                            },
                                        );
                                    },
                                );
                            }
                            UiCommand::OmniboxSelect(idx) => {
                                popup_manager.update_data(&format!("{{\"selectedIndex\":{idx}}}"));
                            }
                            UiCommand::SwitchTab(tab_id) => {
                                if popup_manager.is_open() {
                                    popup_manager.hide_now();
                                    let _ = chrome_webview.evaluate_script(
                                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                                    );
                                }
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::NextTab => {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::PrevTab => {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::SwitchTabIndex(num) => {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::CloseTab(tab_id) => {
                                if tabs.close_tab(tab_id) == CloseTabOutcome::LastTabClosed {
                                    *control_flow = ControlFlow::Exit;
                                } else {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::DuplicateTab(tab_id) => {
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
                                        &find_state,
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
                                        &find_state,
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::ReopenClosedTab => {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::Navigate(raw_input) => {
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
                                    &raw_input,
                                );
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::Back => {
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ = webview.evaluate_script("window.history.back();");
                                    }
                                }
                            }
                            UiCommand::Forward => {
                                if let Some(active) = tabs.active_tab() {
                                    if let Some(webview) = &active.webview {
                                        let _ =
                                            webview.evaluate_script("window.history.forward();");
                                    }
                                }
                            }
                            UiCommand::Reload => {
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::Home => {
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
                                    &find_state,
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
                                        &find_state,
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
                                    &find_state,
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
                                    &find_state,
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
                                    &find_state,
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
                                    &find_state,
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                            UiCommand::ToggleAdblock => {
                                adblocker.toggle();
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            UiCommand::WindowDrag => {
                                let _ = window.drag_window();
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
                                    &find_state,
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                        }
                    }
                }
                AppEvent::PageIpc { tab_id, payload } => {
                    if let Some(page_event) = ui::parse_page_ipc_event(&payload) {
                        match page_event {
                            PageIpcEvent::Favicon(fav_url) => {
                                if tabs.update_favicon(tab_id, Some(fav_url)) {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::Scroll(x, y) => {
                                tabs.update_scroll_position(tab_id, x, y);
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ContextOpenNewTab(target_url) => {
                                if !adblocker.should_block(&target_url) {
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
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::ContextCopyLink(href) => {
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
                                    &raw_target,
                                );
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::HistoryClear => {
                                storage.clear_history();
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                            }
                            PageIpcEvent::HistoryDelete(del_url) => {
                                storage.delete_history_url(&del_url);
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                            }
                            PageIpcEvent::BookmarkRemove(rem_url) => {
                                storage.remove_bookmark(&rem_url);
                                reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutNewTab => {
                                tabs.create_tab(&settings.get().homepage, true);
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::ShortcutCloseTab => {
                                let active_id = tabs.active_tab_id();
                                if tabs.close_tab(active_id) == CloseTabOutcome::LastTabClosed {
                                    *control_flow = ControlFlow::Exit;
                                } else {
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
                                        &find_state,
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
                                        &find_state,
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
                                        &find_state,
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
                                        &find_state,
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
                                        &find_state,
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
                                    &find_state,
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
                                        &find_state,
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
                                    &find_state,
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
                                    &find_state,
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                            PageIpcEvent::PointerDown => {
                                if popup_manager.is_open() {
                                    popup_manager.close(true);
                                    let _ = chrome_webview.evaluate_script(
                                        "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                                    );
                                }
                            }
                            PageIpcEvent::SpaNav(spa_url) => {
                                let changed = tabs.update_page_load(tab_id, spa_url.clone(), false);
                                if let Some(tab) = tabs.get_tab(tab_id) {
                                    storage.record_visit_async(&tab.url, &tab.title);
                                }
                                if changed {
                                    sync_chrome_ui(
                                        &chrome_webview,
                                        &window,
                                        &tabs,
                                        &storage,
                                        &adblocker,
                                        &settings,
                                        &find_state,
                                        &downloads,
                                    );
                                }
                            }
                            PageIpcEvent::SettingsSet { key, value } => {
                                let mut theme_to_apply = None;
                                let mut suspend_to_apply = None;
                                let mut reload_needed = false;

                                settings.update(|s| match key.as_str() {
                                    "search_engine" => {
                                        s.search_engine = SearchEngine::parse(&value);
                                    }
                                    "homepage" => {
                                        s.homepage = value.clone();
                                    }
                                    "theme" => {
                                        let theme = ThemePreference::parse(&value);
                                        s.theme = theme;
                                        theme_to_apply = Some(theme);
                                    }
                                    "tab_suspend_minutes" => {
                                        if let Ok(mins) = value.parse::<u64>() {
                                            s.tab_suspend_minutes = mins;
                                            suspend_to_apply =
                                                Some(Duration::from_secs(mins.saturating_mul(60)));
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
                                    tabs.for_each_webview(|wv| ui::apply_webview_theme(wv, theme));
                                }
                                if let Some(suspend) = suspend_to_apply {
                                    tabs.set_suspend_timeout(suspend);
                                }
                                if reload_needed
                                    && tabs
                                        .active_tab()
                                        .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                {
                                    reload_active_tab(&mut tabs, &storage, &settings, &adblocker);
                                }
                                sync_chrome_ui(
                                    &chrome_webview,
                                    &window,
                                    &tabs,
                                    &storage,
                                    &adblocker,
                                    &settings,
                                    &find_state,
                                    &downloads,
                                );
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
                                            .active_tab()
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_active_tab(
                                                &mut tabs, &storage, &settings, &adblocker,
                                            );
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
                                            .active_tab()
                                            .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                        {
                                            reload_active_tab(
                                                &mut tabs, &storage, &settings, &adblocker,
                                            );
                                        }
                                    }
                                    "allowlist_add" => {
                                        let host = arg.trim();
                                        if !host.is_empty() {
                                            adblocker.set_site_allowlisted(host, true);
                                            if tabs
                                                .active_tab()
                                                .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                            {
                                                reload_active_tab(
                                                    &mut tabs, &storage, &settings, &adblocker,
                                                );
                                            }
                                        }
                                    }
                                    "allowlist_remove" => {
                                        let host = arg.trim();
                                        if !host.is_empty() {
                                            adblocker.set_site_allowlisted(host, false);
                                            if tabs
                                                .active_tab()
                                                .is_some_and(|t| t.url == pages::SETTINGS_URL)
                                            {
                                                reload_active_tab(
                                                    &mut tabs, &storage, &settings, &adblocker,
                                                );
                                            }
                                        }
                                    }
                                    "open" => {
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
                                            &arg,
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
                                    &find_state,
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
                                    &find_state,
                                    &downloads,
                                );
                            }
                        }
                    }
                }
                AppEvent::TabTitleChanged { tab_id, title } => {
                    if let Some(tab) = tabs.get_tab(tab_id) {
                        storage.update_visit_title_async(&tab.url, &title);
                    }
                    if tabs.update_title(tab_id, title) {
                        sync_chrome_ui(
                            &chrome_webview,
                            &window,
                            &tabs,
                            &storage,
                            &adblocker,
                            &settings,
                            &find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::TabPageLoad {
                    tab_id,
                    url,
                    is_loading,
                } => {
                    let changed = tabs.update_page_load(tab_id, url, is_loading);
                    if !is_loading {
                        if let Some(tab) = tabs.get_tab(tab_id) {
                            storage.record_visit_async(&tab.url, &tab.title);
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
                            &find_state,
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
                AppEvent::OpenInNewTab(url) => {
                    if !adblocker.should_block(&url) {
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
                            &find_state,
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
                            &find_state,
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
                            &find_state,
                            &downloads,
                        );
                    }
                }
                AppEvent::OmniboxSuggestions {
                    query_id,
                    suggestions,
                } => {
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
                        items_json.push('}');
                    }
                    items_json.push(']');
                    let script = format!(
                        "if(window.__emberShowOmniboxSuggestions){{window.__emberShowOmniboxSuggestions({query_id},{items_json});}}"
                    );
                    let _ = chrome_webview.evaluate_script(&script);
                }
                AppEvent::PopupIpc(payload) => {
                    let trimmed = payload.trim();
                    if trimmed == "popup_closed" {
                        popup_manager.hide_now();
                    } else if trimmed == "popup_close_request" {
                        popup_manager.close(true);
                        let _ = chrome_webview.evaluate_script(
                            "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                        );
                    } else if let Some(cmd) = trimmed.strip_prefix("popup_keep_open:") {
                        match cmd.trim() {
                            "zoom_in" => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_in);
                            }
                            "zoom_out" => {
                                adjust_active_tab_zoom(&tabs, &storage, zoom::zoom_out);
                            }
                            "zoom_reset" => {
                                adjust_active_tab_zoom(&tabs, &storage, |_| zoom::zoom_reset());
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
                            &find_state,
                            &downloads,
                        );
                        let zoom_pct = tabs
                            .active_tab()
                            .map(|t| resolve_zoom_for_url(&storage, &t.url))
                            .map_or(100, zoom::zoom_percent);
                        popup_manager.update_data(&format!("{{\"zoomPercent\":{zoom_pct}}}"));
                    } else if let Some(cmd) = trimmed.strip_prefix("popup_action:") {
                        popup_manager.hide_now();
                        let _ = chrome_webview.evaluate_script(
                            "if(window.__emberOnPopupClosed){window.__emberOnPopupClosed();}",
                        );
                        let action = cmd.trim();
                        if action == "open_find" {
                            let _ = chrome_webview.focus();
                            let _ = chrome_webview.evaluate_script(
                                "if(window.__emberOpenFind){window.__emberOpenFind();}",
                            );
                        } else {
                            let _ = proxy.send_event(AppEvent::UiIpc(action.to_owned()));
                        }
                    }
                }
            },
            _ => {}
        }
    });
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
