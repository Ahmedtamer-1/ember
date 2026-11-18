pub mod resources;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc, RwLock,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use adblock::{
    cosmetic_filter_cache::UrlSpecificResources,
    lists::{FilterSet, ParseOptions},
    request::Request,
    Engine,
};

/// Official EasyList filter list URL.
pub const EASYLIST_URL: &str = "https://easylist.to/easylist/easylist.txt";

/// Official EasyPrivacy filter list URL.
pub const EASYPRIVACY_URL: &str = "https://easylist.to/easylist/easyprivacy.txt";

/// uBlock Origin core filters URL.
pub const UBLOCK_FILTERS_URL: &str = "https://ublockorigin.github.io/uAssets/filters/filters.txt";

/// uBlock Origin unbreak filters URL.
pub const UBLOCK_UNBREAK_URL: &str = "https://ublockorigin.github.io/uAssets/filters/unbreak.txt";

/// uBlock Origin quick-fixes filters URL (tracks YouTube and fast-moving site changes).
pub const UBLOCK_QUICK_FIXES_URL: &str =
    "https://ublockorigin.github.io/uAssets/filters/quick-fixes.txt";

/// uBlock Origin annoyances filters URL.
pub const UBLOCK_ANNOYANCES_URL: &str =
    "https://ublockorigin.github.io/uAssets/filters/annoyances-others.txt";

/// Interval (24 hours) between automatic background updates for standard filter lists.
pub const FILTER_UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Interval (6 hours) between automatic background updates for `uBlock filters - Quick fixes`.
pub const QUICK_FIXES_UPDATE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Serialized engine cache filenames inside `%LOCALAPPDATA%\Ember\filters`.
const ENGINE_CACHE_FILE: &str = "engine-v2.dat";
const EASYLIST_CACHE_FILE: &str = "easylist.txt";
const EASYPRIVACY_CACHE_FILE: &str = "easyprivacy.txt";
const UBLOCK_FILTERS_CACHE_FILE: &str = "ublock-filters.txt";
const UBLOCK_UNBREAK_CACHE_FILE: &str = "ublock-unbreak.txt";
const UBLOCK_QUICK_FIXES_CACHE_FILE: &str = "ublock-quick-fixes.txt";
const UBLOCK_ANNOYANCES_CACHE_FILE: &str = "ublock-annoyances.txt";
const LAST_UPDATED_FILE: &str = "last_updated.txt";
const LAST_QUICK_FIXES_UPDATED_FILE: &str = "last_updated_quick_fixes.txt";
const ALLOWLIST_FILE: &str = "allowlist.txt";

/// Embedded bootstrap rules (EasyList + EasyPrivacy core network & cosmetic rules)
/// used immediately on first launch so startup never waits for network downloads.
pub const DEFAULT_BOOTSTRAP_RULES: &[&str] = &[
    // Third-party ad networks & programmatic exchanges
    "||doubleclick.net^$third-party",
    "||googlesyndication.com^$third-party",
    "||googleadservices.com^$third-party",
    "||adservice.google.com^$third-party",
    "||pagead2.googlesyndication.com^",
    "||securepubads.g.doubleclick.net^",
    "||tpc.googlesyndication.com^",
    "||adnxs.com^$third-party",
    "||adsrvr.org^$third-party",
    "||criteo.com^$third-party",
    "||criteo.net^$third-party",
    "||outbrain.com^$third-party",
    "||taboola.com^$third-party",
    "||rubiconproject.com^$third-party",
    "||pubmatic.com^$third-party",
    "||openx.net^$third-party",
    "||casalemedia.com^$third-party",
    "||amazon-adsystem.com^$third-party",
    "||moatads.com^$third-party",
    "||adsafeprotected.com^$third-party",
    "||2mdn.net^$third-party",
    "||adform.net^$third-party",
    "||smartadserver.com^$third-party",
    "||advertising.com^$third-party",
    "||quantserve.com^$third-party",
    "||teads.tv^$third-party",
    "||yieldmo.com^$third-party",
    "||sharethrough.com^$third-party",
    "||indexexchange.com^$third-party",
    "||3lift.com^$third-party",
    "||bidswitch.net^$third-party",
    "||medianet.com^$third-party",
    "||media.net^$third-party",
    // EasyPrivacy trackers & analytics beacons
    "||google-analytics.com^$third-party",
    "||googletagmanager.com^$third-party",
    "||googletagservices.com^$third-party",
    "||scorecardresearch.com^$third-party",
    "||omtrdc.net^$third-party",
    "||demdex.net^$third-party",
    "||krxd.net^$third-party",
    "||bluekai.com^$third-party",
    "||exelator.com^$third-party",
    "||newrelic.com^$third-party",
    "||nr-data.net^$third-party",
    "||hotjar.com^$third-party",
    "||Chartbeat.com^$third-party",
    "||chartbeat.net^$third-party",
    "||segment.io^$third-party",
    "||segment.com^$third-party",
    "||mixpanel.com^$third-party",
    "||optimizely.com^$third-party",
    "||fullstory.com^$third-party",
    "||amplitude.com^$third-party",
    "||Clarity.ms^$third-party",
    "||connect.facebook.net/en_US/fbevents.js$script",
    // Common URL path patterns for ad scripts & pixels
    "/pagead/js/adsbygoogle.js",
    "/gpt/pubads_impl_",
    "/ads/ad.js$script",
    "/adserver.",
    "/adframe.",
    "/sponsored-content/",
    // Cosmetic hiding rules (specific & generic)
    "##.ad-container",
    "##.ad-wrapper",
    "##.ad-slot",
    "##.ad-banner",
    "##.advertisement",
    "##.dfp-ad",
    "##.outbrain-tm",
    "##.taboola-widget",
    "###ad-leaderboard",
    "###ad-sidebar",
    "###google_ads_iframe",
    "##ins.adsbygoogle",
    "##div[id^=\"google_ads_iframe_\"]",
    "##iframe[src*=\"doubleclick.net\"]",
    "##iframe[src*=\"googlesyndication.com\"]",
];

/// Script injected via `AddScriptToExecuteOnDocumentCreated` on every document to:
/// 1. Request hostname-specific cosmetic CSS rules as soon as the document starts creating.
/// 2. Lazily observe new DOM classes and IDs via `MutationObserver` and query generic hide rules.
pub const COSMETIC_DOCUMENT_CREATED_SCRIPT: &str = r#"
(function () {
  if (window.__emberCosmeticInitialized) return;
  window.__emberCosmeticInitialized = true;

  var styleEl = null;
  var appliedSelectors = Object.create(null);
  var seenTokens = Object.create(null);
  var pendingClasses = [];
  var pendingIds = [];
  var genericHideDisabled = false;
  var flushTimer = null;

  window.__emberExecutedScriptlets = window.__emberExecutedScriptlets || [];
  var loggedScriptlets = Object.create(null);

  // Iframes cannot use the top page's preloaded rules; they request their own via the
  // per-frame WebView2 `WebMessageReceived` channel (`chrome.webview.postMessage`).
  var isTopFrame = window === window.top;

  function postIpc(msg) {
    if (!isTopFrame && window.chrome && window.chrome.webview && typeof window.chrome.webview.postMessage === "function") {
      window.chrome.webview.postMessage(msg);
    } else if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(msg);
    } else if (window.chrome && window.chrome.webview && typeof window.chrome.webview.postMessage === "function") {
      window.chrome.webview.postMessage(msg);
    }
  }

  window.__emberLogScriptlet = function (name, detail) {
    var key = String(name || "") + ":" + String(detail || "");
    if (loggedScriptlets[key]) return;
    loggedScriptlets[key] = true;
    window.__emberExecutedScriptlets.push({ name: name, detail: detail || "", url: location.href });
    postIpc("scriptlet_log:" + JSON.stringify({ url: location.href, name: String(name || ""), detail: String(detail || "") }));
  };

  function ensureStyleElement() {
    if (styleEl && styleEl.isConnected) return styleEl;
    styleEl = document.getElementById("__ember-cosmetic-style");
    if (!styleEl) {
      styleEl = document.createElement("style");
      styleEl.id = "__ember-cosmetic-style";
      styleEl.type = "text/css";
      var root = document.head || document.documentElement;
      if (root) {
        root.appendChild(styleEl);
      }
    }
    return styleEl;
  }

  var executedInjectedScripts = Object.create(null);

  window.__emberApplyCosmeticRules = function (selectors, genericHide, scriptlet) {
    genericHideDisabled = Boolean(genericHide);
    if (Array.isArray(selectors) && selectors.length > 0) {
      var newSelectors = [];
      for (var i = 0; i < selectors.length; i++) {
        var sel = selectors[i];
        if (sel && !appliedSelectors[sel]) {
          appliedSelectors[sel] = true;
          newSelectors.push(sel);
        }
      }
      if (newSelectors.length > 0) {
        var cssChunk = newSelectors.join(",\n") + " { display: none !important; }\n";
        var applyChunk = function () {
          var el = ensureStyleElement();
          if (el) {
            el.textContent += cssChunk;
          }
        };
        if (document.head || document.documentElement) {
          applyChunk();
        } else {
          document.addEventListener("DOMContentLoaded", applyChunk, { once: true });
        }
      }
    }
    if (scriptlet && typeof scriptlet === "string" && scriptlet.length > 0) {
      if (!executedInjectedScripts[scriptlet]) {
        executedInjectedScripts[scriptlet] = true;
        try {
          var fn = new Function(scriptlet);
          fn();
        } catch (_) {}
      }
    }
  };

  window.__emberApplyPreloadedCosmetic = function () {
    if (isTopFrame && window.__emberPreloadedCosmetic) {
      try {
        var pre = window.__emberPreloadedCosmetic;
        window.__emberApplyCosmeticRules(pre.selectors || [], Boolean(pre.genericHide), pre.scriptlet || "");
      } catch (_) {}
    }
  };
  window.__emberApplyPreloadedCosmetic();

  function collectFromNode(node) {
    if (genericHideDisabled || !node || node.nodeType !== 1) return;
    if (node.id && typeof node.id === "string") {
      var idVal = node.id.trim();
      if (idVal && !seenTokens['id:' + idVal]) {
        seenTokens['id:' + idVal] = true;
        pendingIds.push(idVal);
      }
    }
    if (node.classList && node.classList.length > 0) {
      for (var i = 0; i < node.classList.length; i++) {
        var cls = node.classList[i];
        if (cls && !seenTokens['cls:' + cls]) {
          seenTokens['cls:' + cls] = true;
          pendingClasses.push(cls);
        }
      }
    }
  }

  function collectSubtree(root) {
    if (genericHideDisabled || !root || root.nodeType !== 1) return;
    collectFromNode(root);
    if (root.querySelectorAll) {
      var els = root.querySelectorAll("[id],[class]");
      for (var i = 0; i < els.length && i < 1500; i++) {
        collectFromNode(els[i]);
      }
    }
  }

  function scheduleFlush() {
    if (genericHideDisabled || flushTimer !== null) return;
    if (pendingClasses.length === 0 && pendingIds.length === 0) return;
    flushTimer = setTimeout(function () {
      flushTimer = null;
      if (genericHideDisabled || (pendingClasses.length === 0 && pendingIds.length === 0)) return;
      var batchClasses = pendingClasses.splice(0, 256);
      var batchIds = pendingIds.splice(0, 256);
      postIpc(
        "cosmetic_tokens:" +
          JSON.stringify({
            url: location.href,
            classes: batchClasses,
            ids: batchIds,
          })
      );
    }, 60);
  }

  var lastCosmeticHref = "";
  function triggerCosmeticRefresh() {
    if (location.protocol !== "http:" && location.protocol !== "https:") return;
    if (location.href === lastCosmeticHref) return;
    lastCosmeticHref = location.href;
    postIpc("cosmetic_init:" + location.href);
    if (document.documentElement) {
      collectSubtree(document.documentElement);
      scheduleFlush();
    }
  }

  function installSpaHooks() {
    if (window.__emberCosmeticSpaHooked) return;
    window.__emberCosmeticSpaHooked = true;
    var wrapHistory = function (fnName) {
      var orig = history[fnName];
      if (typeof orig !== "function") return;
      history[fnName] = function () {
        var ret = orig.apply(this, arguments);
        setTimeout(triggerCosmeticRefresh, 0);
        return ret;
      };
    };
    wrapHistory("pushState");
    wrapHistory("replaceState");
    window.addEventListener("popstate", triggerCosmeticRefresh);
    window.addEventListener("hashchange", triggerCosmeticRefresh);
    window.addEventListener("yt-navigate-finish", function () {
      lastCosmeticHref = "";
      triggerCosmeticRefresh();
    });
  }

  function startObserver() {
    installSpaHooks();
    triggerCosmeticRefresh();
    if (document.documentElement) {
      collectSubtree(document.documentElement);
      scheduleFlush();
      var observer = new MutationObserver(function (mutations) {
        if (genericHideDisabled) return;
        for (var i = 0; i < mutations.length; i++) {
          var m = mutations[i];
          if (m.type === "childList" && m.addedNodes) {
            for (var j = 0; j < m.addedNodes.length; j++) {
              collectSubtree(m.addedNodes[j]);
            }
          } else if (m.type === "attributes" && m.target) {
            collectFromNode(m.target);
          }
        }
        scheduleFlush();
      });
      observer.observe(document.documentElement, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["class", "id"],
      });
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", startObserver, { once: true });
  } else {
    startObserver();
  }
})();
"#;

/// Snapshot of cosmetic filtering rules prepared for a specific document URL.
#[derive(Debug, Clone, Default)]
pub struct CosmeticResources {
    pub hide_selectors: Vec<String>,
    pub exceptions: HashSet<String>,
    pub injected_script: String,
    pub generichide: bool,
}

/// Category of adblock rule match recorded for `ember://adblock-debug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdblockMatchKind {
    NetworkBlocked,
    NetworkRedirected,
    CosmeticSelector,
    ScriptletExecuted,
}

impl AdblockMatchKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::NetworkBlocked => "network-block",
            Self::NetworkRedirected => "redirect",
            Self::CosmeticSelector => "cosmetic",
            Self::ScriptletExecuted => "scriptlet",
        }
    }
}

/// A single recorded adblock match or scriptlet execution for debugging.
#[derive(Debug, Clone)]
pub struct AdblockDebugEntry {
    pub kind: AdblockMatchKind,
    pub rule_or_name: String,
    pub target_url: String,
    pub request_type: String,
}

/// Per-tab adblock debug state.
#[derive(Debug, Clone, Default)]
pub struct AdblockTabDebug {
    pub page_url: String,
    pub entries: VecDeque<AdblockDebugEntry>,
}

/// Result of a detailed network request check against the adblock engine.
#[derive(Debug, Clone, Default)]
pub struct NetworkMatchOutcome {
    pub should_block: bool,
    pub redirect: Option<String>,
    pub matched_rule: Option<String>,
}

const MAX_DEBUG_ENTRIES_PER_TAB: usize = 200;

/// Built-in adblocker powered by Brave's `adblock` (`adblock-rust`) engine.
/// Thread-safe and cheap to clone across WebView2 event callbacks and background threads.
#[derive(Clone)]
pub struct Adblocker {
    enabled: Arc<AtomicBool>,
    engine: Arc<RwLock<Arc<Engine>>>,
    allowlist: Arc<RwLock<HashSet<String>>>,
    filters_dir: Option<PathBuf>,
    easylist_enabled: Arc<AtomicBool>,
    easyprivacy_enabled: Arc<AtomicBool>,
    updating: Arc<AtomicBool>,
    /// Set when `update_now()` is requested while an update is already running: the running
    /// updater performs one more pass with a forced download before releasing `updating`.
    rerun_requested: Arc<AtomicBool>,
    /// Filter-list selection the active engine was compiled for (see `encode_engine_lists`);
    /// `0` means the active engine only contains the embedded bootstrap rules.
    active_engine_lists: Arc<AtomicU8>,
    send_dnt_gpc: Arc<AtomicBool>,
    block_third_party_cookies: Arc<AtomicBool>,
    debug_logs: Arc<RwLock<HashMap<usize, AdblockTabDebug>>>,
    last_web_tab_id: Arc<AtomicUsize>,
}

impl Default for Adblocker {
    fn default() -> Self {
        Self::new()
    }
}

impl Adblocker {
    fn assemble(
        engine: Engine,
        allowlist: HashSet<String>,
        filters_dir: Option<PathBuf>,
        easylist: bool,
        easyprivacy: bool,
        engine_lists: Option<(bool, bool)>,
    ) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            engine: Arc::new(RwLock::new(Arc::new(engine))),
            allowlist: Arc::new(RwLock::new(allowlist)),
            filters_dir,
            easylist_enabled: Arc::new(AtomicBool::new(easylist)),
            easyprivacy_enabled: Arc::new(AtomicBool::new(easyprivacy)),
            updating: Arc::new(AtomicBool::new(false)),
            rerun_requested: Arc::new(AtomicBool::new(false)),
            active_engine_lists: Arc::new(AtomicU8::new(encode_engine_lists(engine_lists))),
            send_dnt_gpc: Arc::new(AtomicBool::new(false)),
            block_third_party_cookies: Arc::new(AtomicBool::new(false)),
            debug_logs: Arc::new(RwLock::new(HashMap::new())),
            last_web_tab_id: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Creates a new active adblocker initialized with the embedded bootstrap filter rules.
    pub fn new() -> Self {
        let engine = compile_engine_from_rules(DEFAULT_BOOTSTRAP_RULES.iter().copied());
        Self::assemble(engine, HashSet::new(), None, true, true, None)
    }

    /// Creates an adblocker backed by `%LOCALAPPDATA%\Ember\filters`:
    /// - Synchronously loads `allowlist.txt` and the serialized engine matching the enabled filter
    ///   lists (or falls back to cached text rules / embedded bootstrap rules so the first launch
    ///   never waits for network downloads).
    /// - Spawns a background thread that downloads EasyList and EasyPrivacy if missing or older
    ///   than 24 hours, compiles and serializes the engine, and swaps it live.
    pub fn with_filters_dir(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> Self {
        let _ = fs::create_dir_all(filters_dir);
        let allowlist_set = load_allowlist_from_disk(&filters_dir.join(ALLOWLIST_FILE));
        let (initial_engine, from_snapshot) =
            load_or_bootstrap_engine(filters_dir, easylist, easyprivacy);

        let blocker = Self::assemble(
            initial_engine,
            allowlist_set,
            Some(filters_dir.to_path_buf()),
            easylist,
            easyprivacy,
            from_snapshot.then_some((easylist, easyprivacy)),
        );

        blocker.spawn_background_updater();
        blocker
    }

    /// Creates an adblocker from custom filter rule strings (used for tests and benchmarks).
    pub fn from_rules(rules: &[&str]) -> Self {
        let engine = compile_engine_from_rules(rules.iter().copied());
        Self::assemble(engine, HashSet::new(), None, true, true, None)
    }

    /// Returns `(easylist_enabled, easyprivacy_enabled)`.
    pub fn lists_enabled(&self) -> (bool, bool) {
        (
            self.easylist_enabled.load(Ordering::Relaxed),
            self.easyprivacy_enabled.load(Ordering::Relaxed),
        )
    }

    /// Enables or disables the EasyList / EasyPrivacy lists and recompiles the engine on a
    /// background thread (downloading a list first only if it has never been cached).
    pub fn set_filter_lists(&self, easylist: bool, easyprivacy: bool) {
        if self.lists_enabled() == (easylist, easyprivacy) {
            return;
        }
        self.easylist_enabled.store(easylist, Ordering::Relaxed);
        self.easyprivacy_enabled
            .store(easyprivacy, Ordering::Relaxed);
        let Some(dir) = self.filters_dir.as_deref() else {
            return;
        };
        let need_download = (easylist && !dir.join(EASYLIST_CACHE_FILE).is_file())
            || (easyprivacy && !dir.join(EASYPRIVACY_CACHE_FILE).is_file());
        self.spawn_rebuild(need_download);
    }

    /// Downloads fresh EasyList / EasyPrivacy copies and recompiles the engine in the background.
    /// If an update is already running, it performs one more forced pass once it finishes.
    pub fn update_now(&self) {
        self.spawn_rebuild(true);
    }

    /// Returns `true` while a filter download / recompilation is in progress.
    pub fn is_updating(&self) -> bool {
        self.updating.load(Ordering::SeqCst)
    }

    /// Unix timestamp (seconds) of the last successful filter download, if any.
    pub fn last_updated_epoch(&self) -> Option<u64> {
        let dir = self.filters_dir.as_deref()?;
        fs::read_to_string(dir.join(LAST_UPDATED_FILE))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    }

    /// Unix timestamp (seconds) of the last successful `uBlock filters - Quick fixes` download.
    pub fn quick_fixes_last_updated_epoch(&self) -> Option<u64> {
        let dir = self.filters_dir.as_deref()?;
        fs::read_to_string(dir.join(LAST_QUICK_FIXES_UPDATED_FILE))
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
    }

    /// Resets per-tab debug entries when a tab begins a new top-level navigation.
    pub fn reset_tab_debug(&self, tab_id: usize, page_url: &str) {
        self.last_web_tab_id.store(tab_id, Ordering::Relaxed);
        if let Ok(mut guard) = self.debug_logs.write() {
            let entry = guard.entry(tab_id).or_default();
            entry.page_url = page_url.to_owned();
            entry.entries.clear();
        }
    }

    /// Records a matched rule, redirect, cosmetic selector, or executed scriptlet for a tab.
    pub fn record_debug_event(
        &self,
        tab_id: usize,
        kind: AdblockMatchKind,
        rule_or_name: String,
        target_url: String,
        request_type: String,
    ) {
        self.last_web_tab_id.store(tab_id, Ordering::Relaxed);
        if let Ok(mut guard) = self.debug_logs.write() {
            let tab_debug = guard.entry(tab_id).or_default();
            if tab_debug.entries.len() >= MAX_DEBUG_ENTRIES_PER_TAB {
                tab_debug.entries.pop_front();
            }
            tab_debug.entries.push_back(AdblockDebugEntry {
                kind,
                rule_or_name,
                target_url,
                request_type,
            });
        }
    }

    /// Returns a snapshot of matched rules and executed scriptlets for `tab_id`.
    pub fn tab_debug_snapshot(&self, tab_id: usize) -> AdblockTabDebug {
        self.debug_logs
            .read()
            .ok()
            .and_then(|g| g.get(&tab_id).cloned())
            .unwrap_or_default()
    }

    /// Returns `(tab_id, snapshot)` for the most recently active web tab.
    pub fn latest_tab_debug_snapshot(&self) -> (usize, AdblockTabDebug) {
        let tab_id = self.last_web_tab_id.load(Ordering::Relaxed);
        (tab_id, self.tab_debug_snapshot(tab_id))
    }
    /// Configures the privacy signals applied inside the `WebResourceRequested` handler.
    pub fn set_privacy_flags(&self, send_dnt_gpc: bool, block_third_party_cookies: bool) {
        self.send_dnt_gpc.store(send_dnt_gpc, Ordering::Relaxed);
        self.block_third_party_cookies
            .store(block_third_party_cookies, Ordering::Relaxed);
    }

    /// Whether `DNT: 1` / `Sec-GPC: 1` request headers are sent.
    pub fn send_dnt_gpc(&self) -> bool {
        self.send_dnt_gpc.load(Ordering::Relaxed)
    }

    /// Whether `Cookie` headers are stripped from third-party subresource requests.
    pub fn block_third_party_cookies(&self) -> bool {
        self.block_third_party_cookies.load(Ordering::Relaxed)
    }

    /// Returns the sorted list of allowlisted hostnames.
    pub fn allowlist_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .allowlist
            .read()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        hosts.sort_unstable();
        hosts
    }

    /// Adds or removes `host_or_url` from the per-site allowlist and persists `allowlist.txt`.
    pub fn set_site_allowlisted(&self, host_or_url: &str, allow: bool) -> bool {
        let Some(host) = extract_normalized_host(host_or_url) else {
            return false;
        };
        let Ok(mut guard) = self.allowlist.write() else {
            return false;
        };
        if allow {
            guard.insert(host);
        } else {
            guard.remove(&host);
        }
        if let Some(dir) = &self.filters_dir {
            save_allowlist_to_disk(&dir.join(ALLOWLIST_FILE), &guard);
        }
        true
    }

    /// Returns whether global adblocking is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Explicitly sets whether global adblocking is enabled.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Toggles global adblocking and returns the new state.
    pub fn toggle(&self) -> bool {
        let prev = self.enabled.fetch_xor(true, Ordering::Relaxed);
        !prev
    }

    /// Returns whether the given page URL (or hostname) is in the per-site allowlist.
    pub fn is_site_allowlisted(&self, page_url_or_host: &str) -> bool {
        let Some(host) = extract_normalized_host(page_url_or_host) else {
            return false;
        };
        self.allowlist
            .read()
            .ok()
            .is_some_and(|set| set.contains(&host))
    }

    /// Toggles per-site allowlist status for `page_url_or_host`, persists `allowlist.txt`,
    /// and returns `Some(is_now_allowlisted)`.
    pub fn toggle_site_allowlist(&self, page_url_or_host: &str) -> Option<bool> {
        let host = extract_normalized_host(page_url_or_host)?;
        let mut guard = self.allowlist.write().ok()?;
        let now_allowlisted = if guard.remove(&host) {
            false
        } else {
            guard.insert(host);
            true
        };
        if let Some(dir) = &self.filters_dir {
            save_allowlist_to_disk(&dir.join(ALLOWLIST_FILE), &guard);
        }
        Some(now_allowlisted)
    }

    /// Detailed network match outcome including optional `$redirect` data URL and matched rule string.
    pub fn check_network_request_detailed(
        &self,
        request_url: &str,
        source_url: &str,
        request_type: &str,
    ) -> NetworkMatchOutcome {
        if !self.is_enabled() {
            return NetworkMatchOutcome::default();
        }

        let req_trimmed = request_url.trim();
        if !(req_trimmed.starts_with("http://")
            || req_trimmed.starts_with("https://")
            || req_trimmed.starts_with("ws://")
            || req_trimmed.starts_with("wss://"))
        {
            return NetworkMatchOutcome::default();
        }

        let effective_source = if source_url.trim().starts_with("http://")
            || source_url.trim().starts_with("https://")
        {
            source_url.trim()
        } else {
            req_trimmed
        };

        // Fast-path: check per-site allowlist only if any site is currently allowlisted.
        if let Ok(guard) = self.allowlist.read() {
            if !guard.is_empty() {
                if let Some(source_host) = extract_normalized_host_str(effective_source) {
                    if guard.contains(source_host) {
                        return NetworkMatchOutcome::default();
                    }
                    if source_host.bytes().any(|b| b.is_ascii_uppercase()) {
                        let lower = source_host.to_ascii_lowercase();
                        if guard.contains(&lower) {
                            return NetworkMatchOutcome::default();
                        }
                    }
                }
            }
        }

        // Clone the `Arc<Engine>` pointer and drop the read lock before running the match.
        let Some(engine) = self.engine.read().ok().map(|guard| Arc::clone(&guard)) else {
            return NetworkMatchOutcome::default();
        };

        let Ok(request) = Request::new(req_trimmed, effective_source, request_type, "get") else {
            return NetworkMatchOutcome::default();
        };

        let result = engine.check_network_request(&request);
        let should_block = result.should_block();
        let matched_rule = if should_block {
            result
                .filter
                .as_ref()
                .map(|f| f.raw_line.clone().unwrap_or_else(|| f.to_string()))
        } else {
            None
        };
        NetworkMatchOutcome {
            should_block,
            redirect: if should_block { result.redirect } else { None },
            matched_rule,
        }
    }

    /// Fast, non-blocking network request check suitable for WebView2's `WebResourceRequested`.
    /// Performs zero disk or network I/O and holds no locks during engine evaluation.
    pub fn check_network_request(
        &self,
        request_url: &str,
        source_url: &str,
        request_type: &str,
    ) -> bool {
        self.check_network_request_detailed(request_url, source_url, request_type)
            .should_block
    }

    /// Convenience helper for top-level navigation checks.
    pub fn should_block(&self, url: &str) -> bool {
        self.check_network_request(url, url, "document")
    }

    /// Computes hostname-specific cosmetic resources (`hide_selectors`, `exceptions`, `injected_script`, `generichide`)
    /// for a document URL. Returns empty resources if adblocking is disabled or the site is allowlisted.
    pub fn cosmetic_resources_for_url(&self, url: &str) -> CosmeticResources {
        if !self.is_enabled() || self.is_site_allowlisted(url) {
            return CosmeticResources {
                generichide: true,
                ..CosmeticResources::default()
            };
        }

        let Some(engine) = self.engine.read().ok().map(|guard| Arc::clone(&guard)) else {
            return CosmeticResources::default();
        };

        let UrlSpecificResources {
            hide_selectors,
            exceptions,
            injected_script,
            generichide,
            ..
        } = engine.url_cosmetic_resources(url);

        CosmeticResources {
            hide_selectors: hide_selectors.into_iter().collect(),
            exceptions,
            injected_script,
            generichide,
        }
    }

    /// Queries generic hide rules lazily for newly observed DOM classes and IDs on a page.
    pub fn hidden_class_id_selectors(
        &self,
        classes: &[String],
        ids: &[String],
        exceptions: &HashSet<String>,
    ) -> Vec<String> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let Some(engine) = self.engine.read().ok().map(|guard| Arc::clone(&guard)) else {
            return Vec::new();
        };
        engine.hidden_class_id_selectors(classes, ids, exceptions)
    }

    /// Serializes the currently active `Engine` into a binary buffer (`engine.serialize()`).
    #[cfg(test)]
    pub fn serialize_engine(&self) -> Option<Vec<u8>> {
        let engine = self.engine.read().ok().map(|guard| Arc::clone(&guard))?;
        Some(engine.serialize())
    }

    /// Deserializes a compiled `Engine` from bytes and swaps it into this `Adblocker`.
    #[cfg(test)]
    pub fn deserialize_engine(&self, bytes: &[u8]) -> bool {
        let mut new_engine = Engine::default();
        if new_engine.deserialize(bytes).is_ok() {
            if let Ok(mut guard) = self.engine.write() {
                *guard = Arc::new(new_engine);
                return true;
            }
        }
        false
    }

    fn spawn_background_updater(&self) {
        let this = self.clone();
        thread::Builder::new()
            .name(String::from("ember-adblock-updater"))
            .spawn(move || loop {
                let (easylist, easyprivacy) = this.lists_enabled();
                if let Some(dir) = this.filters_dir.as_deref() {
                    let plan = refresh_plan_for_dir(dir, easylist, easyprivacy);
                    if plan.any() {
                        this.run_rebuild_with_plan(plan, false);
                    }
                }
                thread::sleep(Duration::from_secs(3600));
            })
            .ok();
    }

    fn spawn_rebuild(&self, force_download: bool) {
        let this = self.clone();
        thread::Builder::new()
            .name(String::from("ember-adblock-rebuild"))
            .spawn(move || this.run_rebuild(force_download))
            .ok();
    }

    /// Blocking rebuild: (optionally) downloads the enabled lists, recompiles the engine for the
    /// currently selected lists and swaps it in. Re-runs if the selection changed mid-build.
    fn run_rebuild(&self, force_download: bool) {
        let Some(dir) = self.filters_dir.clone() else {
            return;
        };
        let plan = if force_download {
            RefreshPlan::ALL
        } else {
            let (easylist, easyprivacy) = self.lists_enabled();
            refresh_plan_for_dir(&dir, easylist, easyprivacy)
        };
        self.run_rebuild_with_plan(plan, force_download);
    }

    /// Runs rebuild passes until the active engine matches the current list selection and no
    /// forced re-run is pending. If another updater already holds `updating`, a forced request
    /// is handed over via `rerun_requested` instead of being dropped.
    fn run_rebuild_with_plan(&self, mut plan: RefreshPlan, mut forced: bool) {
        let Some(dir) = self.filters_dir.clone() else {
            return;
        };
        loop {
            if self.updating.swap(true, Ordering::SeqCst) {
                if forced {
                    self.rerun_requested.store(true, Ordering::SeqCst);
                }
                return;
            }

            let mut built_with = self.rebuild_pass(&dir, plan);
            loop {
                let rerun = self.rerun_requested.swap(false, Ordering::SeqCst);
                match next_rebuild_action(built_with, self.lists_enabled(), rerun) {
                    RebuildNext::Done => break,
                    RebuildNext::Rerun {
                        force_download,
                        lists,
                    } => {
                        let next_plan = if force_download {
                            RefreshPlan::ALL
                        } else {
                            refresh_plan_for_dir(&dir, lists.0, lists.1)
                        };
                        built_with = self.rebuild_pass(&dir, next_plan);
                    }
                }
            }
            self.updating.store(false, Ordering::SeqCst);

            // A request may have arrived between the last check and releasing `updating`.
            let rerun = self.rerun_requested.swap(false, Ordering::SeqCst);
            match next_rebuild_action(built_with, self.lists_enabled(), rerun) {
                RebuildNext::Done => return,
                RebuildNext::Rerun {
                    force_download,
                    lists,
                } => {
                    forced = force_download;
                    plan = if force_download {
                        RefreshPlan::ALL
                    } else {
                        refresh_plan_for_dir(&dir, lists.0, lists.1)
                    };
                }
            }
        }
    }

    /// One download/compile pass for the currently selected lists. Falls back to compiling from
    /// the cached list files when every download failed (e.g. offline) but the active engine was
    /// built for a different list selection. Returns the list selection the pass targeted.
    fn rebuild_pass(&self, dir: &Path, plan: RefreshPlan) -> (bool, bool) {
        let lists = self.lists_enabled();
        let (easylist, easyprivacy) = lists;
        let downloaded = if plan.any() {
            download_and_compile_filters(
                dir,
                easylist,
                easyprivacy,
                plan.standard,
                plan.quick_fixes,
            )
        } else {
            None
        };
        let active = decode_engine_lists(self.active_engine_lists.load(Ordering::SeqCst));
        let engine = match downloaded {
            Some(engine) => Some(engine),
            None if needs_cache_compile(active, lists) => {
                Some(compile_and_persist_engine(dir, easylist, easyprivacy))
            }
            None => None,
        };
        if let Some(engine) = engine {
            if let Ok(mut guard) = self.engine.write() {
                *guard = Arc::new(engine);
                self.active_engine_lists
                    .store(encode_engine_lists(Some(lists)), Ordering::SeqCst);
            }
        }
        lists
    }
}

/// Encodes the list selection an engine was compiled for into a single byte (`0` = none).
fn encode_engine_lists(lists: Option<(bool, bool)>) -> u8 {
    match lists {
        None => 0,
        Some((easylist, easyprivacy)) => 0b100 | (u8::from(easylist) << 1) | u8::from(easyprivacy),
    }
}

fn decode_engine_lists(raw: u8) -> Option<(bool, bool)> {
    if raw & 0b100 == 0 {
        None
    } else {
        Some((raw & 0b10 != 0, raw & 0b1 != 0))
    }
}

/// Whether an engine must be recompiled from the cached list files because no download produced
/// one and the active engine does not match the wanted list selection.
fn needs_cache_compile(active: Option<(bool, bool)>, wanted: (bool, bool)) -> bool {
    active != Some(wanted)
}

/// What the rebuild loop should do after a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RebuildNext {
    Done,
    /// Run another pass for `lists` (the *current* selection), downloading everything first if
    /// `force_download` is set.
    Rerun {
        force_download: bool,
        lists: (bool, bool),
    },
}

/// Decides whether another rebuild pass is needed. The next pass always targets `lists_now`
/// (never the selection the previous pass was built with).
fn next_rebuild_action(
    built_with: (bool, bool),
    lists_now: (bool, bool),
    rerun_requested: bool,
) -> RebuildNext {
    if rerun_requested {
        RebuildNext::Rerun {
            force_download: true,
            lists: lists_now,
        }
    } else if lists_now != built_with {
        RebuildNext::Rerun {
            force_download: false,
            lists: lists_now,
        }
    } else {
        RebuildNext::Done
    }
}

/// Returns `true` if every requested download (`(requested, succeeded)`) succeeded.
fn all_requested_succeeded(results: &[(bool, bool)]) -> bool {
    results.iter().all(|&(requested, ok)| !requested || ok)
}

/// Maps a WebView2 `COREWEBVIEW2_WEB_RESOURCE_CONTEXT` integer value to the corresponding
/// `adblock` request type string.
pub fn map_webview2_resource_context(raw_context: i32, is_subframe_document: bool) -> &'static str {
    match raw_context {
        1 => {
            if is_subframe_document {
                "subdocument"
            } else {
                "document"
            }
        }
        2 => "stylesheet",
        3 => "image",
        4 | 9 => "media",
        5 => "font",
        6 => "script",
        7 => "xmlhttprequest",
        8 => "fetch",
        11 => "websocket",
        14 => "ping",
        15 => "csp_report",
        _ => "other",
    }
}

/// Top-level document state of one tab, tracked across `NavigationStarting` and
/// `WebResourceRequested` to tell main-frame navigations apart from iframe documents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopLevelDocState {
    /// URL of the top-level document whose subresources are currently loading.
    pub committed_url: String,
    /// URI of the top-level navigation announced by `NavigationStarting` whose document request
    /// has not been seen yet.
    pub pending_nav_url: Option<String>,
}

impl TopLevelDocState {
    /// Records a top-level `NavigationStarting` (including redirects).
    pub fn begin_navigation(&mut self, uri: &str) {
        self.pending_nav_url = Some(uri.to_owned());
    }

    /// Records that the main-frame document request for `url` was seen.
    pub fn commit(&mut self, url: &str) {
        self.committed_url.clear();
        self.committed_url.push_str(url);
        self.pending_nav_url = None;
    }
}

/// Classification of a single `WebResourceRequested` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestClassification {
    /// `adblock` request type (`"document"`, `"subdocument"`, `"script"`, ...).
    pub request_type: &'static str,
    /// Source (top-level page) URL to match the request against; may be empty.
    pub source_url: String,
    /// `true` for the main-frame document of a (new) top-level navigation.
    pub is_main_document: bool,
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn strip_fragment(url: &str) -> &str {
    url.split_once('#').map_or(url, |(base, _)| base)
}

/// Classifies a WebView2 request.
///
/// * `context` is the raw `COREWEBVIEW2_WEB_RESOURCE_CONTEXT` value (1 = document).
/// * `state` is the tab's top-level document state (`NavigationStarting` updates it).
/// * `fallback_top_url` is only called when no top-level URL has been recorded yet (e.g. the
///   first navigation began before the handlers were attached) and should return the
///   WebView's current `Source`.
///
/// A document request is the main frame when it matches the pending top-level navigation URI
/// (WebView2's `Source` still points at the *previous* page at that time), or when there is no
/// HTTP(S) top-level page yet, or when it equals the current top-level page (reload). Any other
/// document request is an iframe (`subdocument`) whose source is the top-level page.
pub fn classify_web_request(
    context: i32,
    request_url: &str,
    state: &TopLevelDocState,
    fallback_top_url: impl FnOnce() -> String,
) -> RequestClassification {
    let is_document = context == 1;
    let main_document = || RequestClassification {
        request_type: "document",
        source_url: request_url.to_owned(),
        is_main_document: true,
    };

    if is_document {
        if let Some(pending) = state.pending_nav_url.as_deref() {
            if strip_fragment(pending) == strip_fragment(request_url) {
                return main_document();
            }
        }
    }

    let top_url = if is_http_url(&state.committed_url) {
        state.committed_url.clone()
    } else {
        fallback_top_url()
    };
    let top_is_http = is_http_url(&top_url);

    if is_document {
        if !top_is_http || strip_fragment(&top_url) == strip_fragment(request_url) {
            main_document()
        } else {
            RequestClassification {
                request_type: map_webview2_resource_context(context, true),
                source_url: top_url,
                is_main_document: false,
            }
        }
    } else {
        RequestClassification {
            request_type: map_webview2_resource_context(context, false),
            source_url: if top_is_http { top_url } else { String::new() },
            is_main_document: false,
        }
    }
}

/// Bookkeeping for the per-tab dynamic document-start preload script.
///
/// `AddScriptToExecuteOnDocumentCreated` completes asynchronously, so an update may start before
/// the previous registration has reported its id. Every update bumps `generation`; a completion
/// whose generation is stale must remove its own script immediately.
#[derive(Debug, Clone, Default)]
pub struct PreloadScriptSlot {
    generation: u64,
    current_id: Option<String>,
}

impl PreloadScriptSlot {
    /// Starts a new update. Returns the generation for the new registration and the id of the
    /// previously registered script (if known) that must be removed now.
    pub fn begin_update(&mut self) -> (u64, Option<String>) {
        self.generation = self.generation.wrapping_add(1);
        (self.generation, self.current_id.take())
    }

    /// Handles a completed registration of `id` for `generation`. Returns a script id that must
    /// be removed: `id` itself when it is stale, or a previously stored id it replaces.
    pub fn complete(&mut self, generation: u64, id: String) -> Option<String> {
        if generation != self.generation {
            Some(id)
        } else {
            self.current_id.replace(id)
        }
    }

    /// Id of the currently registered preload script, if known.
    pub fn current_id(&self) -> Option<&str> {
        self.current_id.as_deref()
    }
}

/// Decodes an RFC 2397 `data:` URL (as produced by adblock-rust `$redirect` resources) into its
/// MIME type (lowercase `type/subtype`, defaulting to `text/plain`) and body bytes. Handles both
/// the `;base64,` form and the percent-encoded plain form. Returns `None` for non-`data:` URLs,
/// a missing `,` separator or invalid base64.
pub fn decode_data_url(data_url: &str) -> Option<(String, Vec<u8>)> {
    let trimmed = data_url.trim();
    let prefix = trimmed.get(..5)?;
    if !prefix.eq_ignore_ascii_case("data:") {
        return None;
    }
    let (header, payload) = trimmed[5..].split_once(',')?;

    let mut params = header.split(';');
    let mime_raw = params.next().unwrap_or_default().trim();
    let is_base64 = params.any(|p| p.trim().eq_ignore_ascii_case("base64"));

    let mime = if is_valid_mime(mime_raw) {
        mime_raw.to_ascii_lowercase()
    } else {
        String::from("text/plain")
    };

    let bytes = percent_decode(payload.as_bytes());
    let body = if is_base64 {
        decode_base64(&bytes)?
    } else {
        bytes
    };
    Some((mime, body))
}

/// `type/subtype` made only of RFC 7230 token characters (so it is safe inside a header line).
fn is_valid_mime(mime: &str) -> bool {
    let is_token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    };
    mime.split_once('/')
        .is_some_and(|(ty, sub)| is_token(ty) && is_token(sub))
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == b'%' {
            if let (Some(hi), Some(lo)) = (
                input.get(i + 1).copied().and_then(hex),
                input.get(i + 2).copied().and_then(hex),
            ) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// Forgiving standard-alphabet base64 decoder (ASCII whitespace ignored, padding optional).
fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let filtered: Vec<u8> = input
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut data: &[u8] = &filtered;
    if data.len().is_multiple_of(4) {
        for _ in 0..2 {
            if let Some(rest) = data.strip_suffix(b"=") {
                data = rest;
            }
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }

    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Response headers for a `$redirect` replacement resource with the given MIME type.
pub fn redirect_response_headers(mime: &str) -> String {
    let mime = if is_valid_mime(mime) {
        mime
    } else {
        "text/plain"
    };
    format!("Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *")
}

/// Builds a tiny synchronous document-start preload script injected via
/// `AddScriptToExecuteOnDocumentCreated` on navigation start so `COSMETIC_DOCUMENT_CREATED_SCRIPT`
/// applies initial selectors and executes scriptlets before page `<script>` tags run.
///
/// The script also runs in every iframe (WebView2 injects document-created scripts into all
/// frames), so it is gated on `window===window.top`: the top page's selectors and scriptlets must
/// never apply inside iframes, which fetch their own rules via `cosmetic_init:`.
pub fn build_document_start_preload_script(resources: &CosmeticResources) -> String {
    let mut script =
        String::from("if(window===window.top){window.__emberPreloadedCosmetic={selectors:[");
    for (idx, sel) in resources.hide_selectors.iter().enumerate() {
        if idx > 0 {
            script.push(',');
        }
        script.push('"');
        push_js_escaped(&mut script, sel);
        script.push('"');
    }
    script.push_str("],genericHide:");
    script.push_str(if resources.generichide {
        "true"
    } else {
        "false"
    });
    script.push_str(",scriptlet:\"");
    push_js_escaped(&mut script, &resources.injected_script);
    script.push_str(
        "\"};if(window.__emberApplyPreloadedCosmetic){window.__emberApplyPreloadedCosmetic();}}",
    );
    script
}

/// Builds a JavaScript invocation string that applies initial hostname-specific cosmetic selectors
/// and scriptlets on a page.
pub fn build_cosmetic_apply_script(resources: &CosmeticResources) -> String {
    let mut script =
        String::from("if(window.__emberApplyCosmeticRules){window.__emberApplyCosmeticRules([");
    for (idx, sel) in resources.hide_selectors.iter().enumerate() {
        if idx > 0 {
            script.push(',');
        }
        script.push('"');
        push_js_escaped(&mut script, sel);
        script.push('"');
    }
    script.push_str("],");
    script.push_str(if resources.generichide {
        "true,\""
    } else {
        "false,\""
    });
    push_js_escaped(&mut script, &resources.injected_script);
    script.push_str("\");}");
    script
}

/// Builds a JavaScript invocation string that appends newly matched generic hide selectors.
pub fn build_generic_selectors_script(selectors: &[String]) -> String {
    if selectors.is_empty() {
        return String::new();
    }
    let mut script =
        String::from("if(window.__emberApplyCosmeticRules){window.__emberApplyCosmeticRules([");
    for (idx, sel) in selectors.iter().enumerate() {
        if idx > 0 {
            script.push(',');
        }
        script.push('"');
        push_js_escaped(&mut script, sel);
        script.push('"');
    }
    script.push_str("],false,\"\");}");
    script
}

/// Max hostname-specific selectors recorded per document in the debug log.
const MAX_DEBUG_SELECTORS_PER_DOCUMENT: usize = 40;

/// Parses a web message posted by `COSMETIC_DOCUMENT_CREATED_SCRIPT` inside an iframe
/// (delivered through the per-frame WebView2 `WebMessageReceived` event).
///
/// Only the cosmetic channel is accepted (`cosmetic_init:`, `cosmetic_tokens:`,
/// `scriptlet_log:`): iframes are untrusted third-party content and must not be able to drive
/// tab shortcuts or any other page IPC.
pub fn parse_frame_cosmetic_message(raw: &str) -> Option<crate::ui::PageIpcEvent> {
    use crate::ui::PageIpcEvent;
    crate::ui::parse_page_ipc_event(raw).filter(|event| {
        matches!(
            event,
            PageIpcEvent::CosmeticInit(_)
                | PageIpcEvent::CosmeticTokens { .. }
                | PageIpcEvent::ScriptletLog { .. }
        )
    })
}

/// Handles one parsed iframe cosmetic message and returns the script to run in *that frame*
/// (via `ICoreWebView2Frame2::ExecuteScript`), recording debug events for `tab_id`.
///
/// `top_url` is the tab's top-level document URL: when that site is allowlisted, iframes on it
/// are left unfiltered as well (the frame's own URL decides the selectors otherwise).
pub fn frame_cosmetic_script(
    adblocker: &Adblocker,
    tab_id: usize,
    top_url: &str,
    event: &crate::ui::PageIpcEvent,
) -> Option<String> {
    use crate::ui::PageIpcEvent;
    let top_allowlisted = is_http_url(top_url) && adblocker.is_site_allowlisted(top_url);
    match event {
        PageIpcEvent::CosmeticInit(url) => {
            if !is_http_url(url) {
                return None;
            }
            if top_allowlisted {
                // Disables generic token reporting in the frame; applies nothing.
                return Some(build_cosmetic_apply_script(&CosmeticResources {
                    generichide: true,
                    ..CosmeticResources::default()
                }));
            }
            let resources = adblocker.cosmetic_resources_for_url(url);
            for sel in resources
                .hide_selectors
                .iter()
                .take(MAX_DEBUG_SELECTORS_PER_DOCUMENT)
            {
                adblocker.record_debug_event(
                    tab_id,
                    AdblockMatchKind::CosmeticSelector,
                    sel.clone(),
                    url.clone(),
                    String::from("iframe-cosmetic"),
                );
            }
            Some(build_cosmetic_apply_script(&resources))
        }
        PageIpcEvent::CosmeticTokens { url, classes, ids } => {
            if top_allowlisted || !is_http_url(url) {
                return None;
            }
            let resources = adblocker.cosmetic_resources_for_url(url);
            if resources.generichide {
                return None;
            }
            let selectors =
                adblocker.hidden_class_id_selectors(classes, ids, &resources.exceptions);
            for sel in &selectors {
                adblocker.record_debug_event(
                    tab_id,
                    AdblockMatchKind::CosmeticSelector,
                    sel.clone(),
                    url.clone(),
                    String::from("iframe-generic-cosmetic"),
                );
            }
            let script = build_generic_selectors_script(&selectors);
            (!script.is_empty()).then_some(script)
        }
        PageIpcEvent::ScriptletLog { url, name, detail } => {
            let label = if detail.is_empty() {
                name.clone()
            } else {
                format!("{name}({detail})")
            };
            adblocker.record_debug_event(
                tab_id,
                AdblockMatchKind::ScriptletExecuted,
                label,
                url.clone(),
                String::from("iframe-scriptlet"),
            );
            None
        }
        _ => None,
    }
}

/// Returns `true` when a main-frame cosmetic message about `message_url` plausibly came from
/// the document at `source_uri` (the sender URI WebView2 reports for the web message).
///
/// Iframe messages are handled per frame by the adblock platform layer; this guards the
/// main-frame path against applying another document's rules (e.g. a message naming a
/// different site). Hosts are compared because SPA `pushState` may change the path but never
/// the origin. An empty/non-web `source_uri` is accepted (nothing to compare against).
pub fn cosmetic_message_matches_source(message_url: &str, source_uri: &str) -> bool {
    if !is_http_url(source_uri) {
        return true;
    }
    if !is_http_url(message_url) {
        return false;
    }
    match (
        extract_normalized_host_str(message_url),
        extract_normalized_host_str(source_uri),
    ) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
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

/// Extracts a borrowed hostname (stripping leading `www.`) from a URL or bare host string,
/// avoiding heap allocation.
pub fn extract_normalized_host_str(url_or_host: &str) -> Option<&str> {
    let trimmed = url_or_host.trim();
    if trimmed.is_empty() {
        return None;
    }

    let after_scheme = if let Some(idx) = trimmed.find("://") {
        let scheme = &trimmed[..idx];
        if !scheme.eq_ignore_ascii_case("http")
            && !scheme.eq_ignore_ascii_case("https")
            && !scheme.eq_ignore_ascii_case("ws")
            && !scheme.eq_ignore_ascii_case("wss")
        {
            return None;
        }
        &trimmed[idx + 3..]
    } else if trimmed.contains(':') && !trimmed.contains('.') {
        return None;
    } else {
        trimmed
    };

    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, right)| right);
    let host = if host_port.starts_with('[') {
        // IPv6 literal: keep the brackets, drop an optional `:port`.
        host_port
            .find(']')
            .map_or(host_port, |end| &host_port[..=end])
    } else {
        host_port.split_once(':').map_or(host_port, |(h, _)| h)
    }
    .trim();

    let normalized = if host.len() >= 4 && host[..4].eq_ignore_ascii_case("www.") {
        &host[4..]
    } else {
        host
    };
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Extracts a lowercase hostname (stripping leading `www.`) from a URL or bare host string.
pub fn extract_normalized_host(url_or_host: &str) -> Option<String> {
    extract_normalized_host_str(url_or_host).map(|h| h.to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RefreshPlan {
    standard: bool,
    quick_fixes: bool,
}

impl RefreshPlan {
    /// Download every list (used by forced updates).
    const ALL: Self = Self {
        standard: true,
        quick_fixes: true,
    };

    fn any(self) -> bool {
        self.standard || self.quick_fixes
    }
}

fn trusted_parse_options() -> ParseOptions {
    ParseOptions {
        permissions: resources::TRUSTED_SCRIPTLET_PERMISSION,
        ..ParseOptions::default()
    }
}

fn compile_engine_from_rules<'a>(rules: impl IntoIterator<Item = &'a str>) -> Engine {
    let mut filter_set = FilterSet::new(true);
    let combined = rules.into_iter().collect::<Vec<_>>().join("\n");
    filter_set.add_filter_list(combined, trusted_parse_options());
    let mut engine = Engine::new_with_filter_set(filter_set);
    engine.use_resources(resources::builtin_resources());
    engine
}

/// Serialized-engine cache file name for a given selection of filter lists.
fn engine_cache_name(easylist: bool, easyprivacy: bool) -> &'static str {
    match (easylist, easyprivacy) {
        (true, true) => ENGINE_CACHE_FILE,
        (true, false) => "engine-easylist-v2.dat",
        (false, true) => "engine-easyprivacy-v2.dat",
        (false, false) => "engine-bootstrap-v2.dat",
    }
}

/// Compiles the embedded bootstrap rules, uBlock Origin lists, and whichever EasyList/EasyPrivacy
/// lists are enabled and cached on disk.
fn compile_engine_from_cache(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> Engine {
    let mut filter_set = FilterSet::new(true);
    let trusted_opts = trusted_parse_options();
    filter_set.add_filter_list(DEFAULT_BOOTSTRAP_RULES.join("\n"), trusted_opts);
    if easylist {
        if let Ok(text) = fs::read_to_string(filters_dir.join(EASYLIST_CACHE_FILE)) {
            filter_set.add_filter_list(text, ParseOptions::default());
        }
    }
    if easyprivacy {
        if let Ok(text) = fs::read_to_string(filters_dir.join(EASYPRIVACY_CACHE_FILE)) {
            filter_set.add_filter_list(text, ParseOptions::default());
        }
    }
    // uBlock Origin trusted lists (core, unbreak, quick-fixes, annoyances)
    for file in [
        UBLOCK_FILTERS_CACHE_FILE,
        UBLOCK_UNBREAK_CACHE_FILE,
        UBLOCK_QUICK_FIXES_CACHE_FILE,
        UBLOCK_ANNOYANCES_CACHE_FILE,
    ] {
        if let Ok(text) = fs::read_to_string(filters_dir.join(file)) {
            filter_set.add_filter_list(text, trusted_opts);
        }
    }
    let mut engine = Engine::new_with_filter_set(filter_set);
    engine.use_resources(resources::builtin_resources());
    engine
}

fn persist_engine(filters_dir: &Path, easylist: bool, easyprivacy: bool, engine: &Engine) {
    let bytes = engine.serialize();
    let name = engine_cache_name(easylist, easyprivacy);
    let tmp_path = filters_dir.join(format!("{name}.tmp"));
    if fs::write(&tmp_path, &bytes).is_ok() {
        let _ = fs::rename(&tmp_path, filters_dir.join(name));
    }
}

fn compile_and_persist_engine(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> Engine {
    let engine = compile_engine_from_cache(filters_dir, easylist, easyprivacy);
    persist_engine(filters_dir, easylist, easyprivacy, &engine);
    engine
}

/// Loads the serialized engine for the selected lists, or the embedded bootstrap rules.
/// The returned flag is `true` when the serialized snapshot was used.
fn load_or_bootstrap_engine(
    filters_dir: &Path,
    easylist: bool,
    easyprivacy: bool,
) -> (Engine, bool) {
    let engine_path = filters_dir.join(engine_cache_name(easylist, easyprivacy));
    if let Ok(bytes) = fs::read(&engine_path) {
        if !bytes.is_empty() {
            let mut engine = Engine::default();
            if engine.deserialize(&bytes).is_ok() {
                engine.use_resources(resources::builtin_resources());
                return (engine, true);
            }
        }
    }

    // No usable snapshot: start with embedded bootstrap rules immediately (<1ms)
    // so cold startup is instant. The background updater will compile/download full lists.
    (
        compile_engine_from_rules(DEFAULT_BOOTSTRAP_RULES.iter().copied()),
        false,
    )
}

fn is_timestamp_stale(path: &Path, max_age: Duration) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };
    let Ok(last_epoch) = contents.trim().parse::<u64>() else {
        return true;
    };
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now_epoch.saturating_sub(last_epoch) >= max_age.as_secs()
}

fn refresh_plan_for_dir(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> RefreshPlan {
    let engine_missing = !filters_dir
        .join(engine_cache_name(easylist, easyprivacy))
        .is_file();
    let standard_missing = (easylist && !filters_dir.join(EASYLIST_CACHE_FILE).is_file())
        || (easyprivacy && !filters_dir.join(EASYPRIVACY_CACHE_FILE).is_file())
        || !filters_dir.join(UBLOCK_FILTERS_CACHE_FILE).is_file()
        || !filters_dir.join(UBLOCK_UNBREAK_CACHE_FILE).is_file()
        || !filters_dir.join(UBLOCK_ANNOYANCES_CACHE_FILE).is_file();
    let quick_fixes_missing = !filters_dir.join(UBLOCK_QUICK_FIXES_CACHE_FILE).is_file();

    let standard_stale =
        is_timestamp_stale(&filters_dir.join(LAST_UPDATED_FILE), FILTER_UPDATE_INTERVAL);
    let quick_fixes_stale = is_timestamp_stale(
        &filters_dir.join(LAST_QUICK_FIXES_UPDATED_FILE),
        QUICK_FIXES_UPDATE_INTERVAL,
    );

    RefreshPlan {
        standard: engine_missing || standard_missing || standard_stale,
        quick_fixes: engine_missing || quick_fixes_missing || quick_fixes_stale,
    }
}

/// Downloads the enabled lists, compiles + serializes the engine and records update timestamps.
/// Returns `None` if every requested download failed.
fn download_and_compile_filters(
    filters_dir: &Path,
    easylist: bool,
    easyprivacy: bool,
    download_standard: bool,
    download_quick_fixes: bool,
) -> Option<Engine> {
    let mut any_ok = false;
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if download_standard {
        let ok_el = easylist
            && platform::download_url_to_file(EASYLIST_URL, &filters_dir.join(EASYLIST_CACHE_FILE));
        let ok_ep = easyprivacy
            && platform::download_url_to_file(
                EASYPRIVACY_URL,
                &filters_dir.join(EASYPRIVACY_CACHE_FILE),
            );
        let ok_uf = platform::download_url_to_file(
            UBLOCK_FILTERS_URL,
            &filters_dir.join(UBLOCK_FILTERS_CACHE_FILE),
        );
        let ok_uu = platform::download_url_to_file(
            UBLOCK_UNBREAK_URL,
            &filters_dir.join(UBLOCK_UNBREAK_CACHE_FILE),
        );
        let ok_ua = platform::download_url_to_file(
            UBLOCK_ANNOYANCES_URL,
            &filters_dir.join(UBLOCK_ANNOYANCES_CACHE_FILE),
        );
        if ok_el || ok_ep || ok_uf || ok_uu || ok_ua {
            any_ok = true;
        }
        // Only record a successful standard update when *every* requested list was refreshed;
        // otherwise the failed lists would not be retried for another 24 hours.
        if all_requested_succeeded(&[
            (easylist, ok_el),
            (easyprivacy, ok_ep),
            (true, ok_uf),
            (true, ok_uu),
            (true, ok_ua),
        ]) {
            let _ = fs::write(filters_dir.join(LAST_UPDATED_FILE), now_epoch.to_string());
        }
    }

    if download_quick_fixes
        && platform::download_url_to_file(
            UBLOCK_QUICK_FIXES_URL,
            &filters_dir.join(UBLOCK_QUICK_FIXES_CACHE_FILE),
        )
    {
        any_ok = true;
        let _ = fs::write(
            filters_dir.join(LAST_QUICK_FIXES_UPDATED_FILE),
            now_epoch.to_string(),
        );
    }

    if !any_ok {
        return None;
    }

    Some(compile_and_persist_engine(
        filters_dir,
        easylist,
        easyprivacy,
    ))
}
fn load_allowlist_from_disk(path: &Path) -> HashSet<String> {
    let Ok(contents) = fs::read_to_string(path) else {
        return HashSet::new();
    };
    contents
        .lines()
        .filter_map(extract_normalized_host)
        .collect()
}

fn save_allowlist_to_disk(path: &Path, set: &HashSet<String>) {
    let mut lines: Vec<&str> = set.iter().map(String::as_str).collect();
    lines.sort_unstable();
    let body = lines.join("\n");
    let _ = fs::write(path, body);
}

/// Measures the current working set memory (in MiB) of the Ember process and its WebView2 child processes.
pub fn measure_process_tree_ram_mb() -> f64 {
    platform::measure_process_tree_ram_mb()
}

/// Attaches native WebView2 `WebResourceRequested` interception and `AddScriptToExecuteOnDocumentCreated`
/// cosmetic & scriptlet filtering to a tab's `wry::WebView`.
pub fn attach_webview_adblocker<F>(
    webview: &wry::WebView,
    tab_id: usize,
    adblocker: Adblocker,
    on_blocked: F,
) where
    F: Fn() + Send + Sync + 'static,
{
    platform::attach_webview_adblocker(webview, tab_id, adblocker, on_blocked);
}

#[cfg(target_os = "windows")]
mod platform {
    use core::ffi::c_void;
    use std::{
        fs,
        path::Path,
        ptr,
        sync::{Arc, Mutex},
    };

    use webview2_com::{
        take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
        ExecuteScriptCompletedHandler, FrameCreatedEventHandler,
        FrameWebMessageReceivedEventHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2Frame, ICoreWebView2Frame2,
            ICoreWebView2WebResourceResponse, ICoreWebView2_2, ICoreWebView2_22, ICoreWebView2_4,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
        },
        NavigationStartingEventHandler, WebResourceRequestedEventHandler,
    };
    use windows_core::{Interface, HSTRING, PWSTR};
    use wry::WebViewExtWindows;

    use super::{
        build_document_start_preload_script, classify_web_request, decode_data_url,
        frame_cosmetic_script, parse_frame_cosmetic_message, redirect_response_headers,
        AdblockMatchKind, Adblocker, PreloadScriptSlot, TopLevelDocState,
        COSMETIC_DOCUMENT_CREATED_SCRIPT, MAX_DEBUG_SELECTORS_PER_DOCUMENT,
    };

    #[link(name = "shlwapi", kind = "raw-dylib")]
    extern "system" {
        /// Returns an owned `IStream*` (or null on failure) holding a copy of `pinit`.
        fn SHCreateMemStream(pinit: *const u8, cbinit: u32) -> *mut c_void;
    }

    #[link(name = "urlmon", kind = "raw-dylib")]
    extern "system" {
        fn URLDownloadToFileW(
            pcaller: *mut c_void,
            sz_url: *const u16,
            sz_file_name: *const u16,
            dw_reserved: u32,
            lpfn_cb: *mut c_void,
        ) -> i32;
    }

    #[repr(C)]
    #[derive(Default)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;

    #[link(name = "kernel32", kind = "raw-dylib")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn CreateToolhelp32Snapshot(dw_flags: u32, th32_process_id: u32) -> isize;
        fn Process32FirstW(h_snapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(h_snapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> isize;
        fn CloseHandle(h_object: isize) -> i32;
        fn K32GetProcessMemoryInfo(
            process: isize,
            ppsmem_counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    fn process_working_set_bytes(pid: u32) -> usize {
        // SAFETY: Opens a query handle to `pid`, reads its `ProcessMemoryCounters`, and closes the handle.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
            if handle == 0 {
                return 0;
            }
            let mut counters = ProcessMemoryCounters {
                cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
                ..ProcessMemoryCounters::default()
            };
            let ok = K32GetProcessMemoryInfo(handle, ptr::addr_of_mut!(counters), counters.cb);
            let _ = CloseHandle(handle);
            if ok != 0 {
                counters.working_set_size
            } else {
                0
            }
        }
    }

    pub fn measure_process_tree_ram_mb() -> f64 {
        // SAFETY: Queries the process table snapshot to sum the working set of Ember and its child processes.
        unsafe {
            let root_pid = GetCurrentProcessId();
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == -1 || snap == 0 {
                return (process_working_set_bytes(root_pid) as f64) / (1024.0 * 1024.0);
            }

            let mut children = Vec::new();
            let mut entry = ProcessEntry32W {
                dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
                cnt_usage: 0,
                th32_process_id: 0,
                th32_default_heap_id: 0,
                th32_module_id: 0,
                cnt_threads: 0,
                th32_parent_process_id: 0,
                pc_pri_class_base: 0,
                dw_flags: 0,
                sz_exe_file: [0u16; 260],
            };

            if Process32FirstW(snap, ptr::addr_of_mut!(entry)) != 0 {
                loop {
                    children.push((entry.th32_process_id, entry.th32_parent_process_id));
                    if Process32NextW(snap, ptr::addr_of_mut!(entry)) == 0 {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);

            let mut tracked_pids = vec![root_pid];
            let mut added = true;
            while added {
                added = false;
                for &(pid, ppid) in &children {
                    if tracked_pids.contains(&ppid) && !tracked_pids.contains(&pid) {
                        tracked_pids.push(pid);
                        added = true;
                    }
                }
            }

            let total_bytes: usize = tracked_pids
                .into_iter()
                .map(process_working_set_bytes)
                .sum();
            (total_bytes as f64) / (1024.0 * 1024.0)
        }
    }

    pub fn download_url_to_file(url: &str, dest: &Path) -> bool {
        let tmp_dest = dest.with_extension("tmp");
        let url_wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        let file_wide: Vec<u16> = tmp_dest
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `url_wide` and `file_wide` are valid null-terminated UTF-16 buffers that outlive
        // the synchronous `URLDownloadToFileW` call on the background updater thread.
        let hr = unsafe {
            URLDownloadToFileW(
                ptr::null_mut(),
                url_wide.as_ptr(),
                file_wide.as_ptr(),
                0,
                ptr::null_mut(),
            )
        };

        if hr == 0 && tmp_dest.is_file() {
            fs::rename(&tmp_dest, dest).is_ok()
        } else {
            let _ = fs::remove_file(&tmp_dest);
            false
        }
    }

    fn update_dynamic_preload_script(
        core_webview: &ICoreWebView2,
        slot: &Arc<Mutex<PreloadScriptSlot>>,
        adblocker: &Adblocker,
        tab_id: usize,
        url: &str,
    ) {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            // Non-web document: drop the previous page's preload script (and invalidate any
            // registration still in flight) so its rules don't run here.
            if let Ok((_, Some(prev_id))) = slot.lock().map(|mut g| g.begin_update()) {
                remove_document_created_script(core_webview, &prev_id);
            }
            return;
        }
        let resources = adblocker.cosmetic_resources_for_url(url);
        for sel in resources
            .hide_selectors
            .iter()
            .take(MAX_DEBUG_SELECTORS_PER_DOCUMENT)
        {
            adblocker.record_debug_event(
                tab_id,
                AdblockMatchKind::CosmeticSelector,
                sel.clone(),
                url.to_owned(),
                String::from("cosmetic"),
            );
        }
        let preload_js = build_document_start_preload_script(&resources);
        let Ok((generation, prev_id)) = slot.lock().map(|mut g| g.begin_update()) else {
            return;
        };
        if let Some(id) = prev_id {
            remove_document_created_script(core_webview, &id);
        }
        let script_hstr = HSTRING::from(preload_js);
        let id_slot = Arc::clone(slot);
        let webview_for_cleanup = core_webview.clone();
        let handler =
            AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(move |hr, id| {
                if hr.is_ok() && !id.is_empty() {
                    let id = id.to_string();
                    let to_remove = match id_slot.lock() {
                        Ok(mut guard) => guard.complete(generation, id),
                        // Poisoned bookkeeping: never leave an untracked script registered.
                        Err(_) => Some(id),
                    };
                    if let Some(stale_id) = to_remove {
                        remove_document_created_script(&webview_for_cleanup, &stale_id);
                    }
                }
                Ok(())
            }));
        // SAFETY: Registers the document-start preload script on `core_webview` for this navigation.
        unsafe {
            let _ = core_webview.AddScriptToExecuteOnDocumentCreated(&script_hstr, &handler);
        }
    }

    /// Subscribes to one iframe's `WebMessageReceived` so its `COSMETIC_DOCUMENT_CREATED_SCRIPT`
    /// instance can request (and receive, via the frame's own `ExecuteScript`) cosmetic rules for
    /// the iframe document's URL instead of the top page's.
    ///
    /// The handler uses the event's `sender` frame rather than capturing `frame`, so there is no
    /// frame -> handler -> frame reference cycle; WebView2 drops the handler with the frame.
    fn attach_frame_cosmetic_handler(
        frame: &ICoreWebView2Frame,
        tab_id: usize,
        adblocker: Adblocker,
        doc_state: Arc<Mutex<TopLevelDocState>>,
    ) {
        let Ok(frame2) = frame.cast::<ICoreWebView2Frame2>() else {
            return;
        };
        let mut token: i64 = 0;
        // SAFETY: Registers a COM callback on a live frame; it is released with the frame.
        unsafe {
            let _ = frame2.add_WebMessageReceived(
                &FrameWebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
                    let (Some(sender), Some(args)) = (sender, args) else {
                        return Ok(());
                    };
                    let mut msg_pwstr = PWSTR::null();
                    if args.TryGetWebMessageAsString(&mut msg_pwstr).is_err() {
                        return Ok(());
                    }
                    let raw = take_pwstr(msg_pwstr);
                    let Some(event) = parse_frame_cosmetic_message(&raw) else {
                        return Ok(());
                    };
                    let top_url = doc_state
                        .lock()
                        .map(|guard| guard.committed_url.clone())
                        .unwrap_or_default();
                    let Some(script) = frame_cosmetic_script(&adblocker, tab_id, &top_url, &event)
                    else {
                        return Ok(());
                    };
                    let Ok(sender2) = sender.cast::<ICoreWebView2Frame2>() else {
                        return Ok(());
                    };
                    let js = HSTRING::from(script);
                    let _ = sender2.ExecuteScript(
                        &js,
                        &ExecuteScriptCompletedHandler::create(Box::new(|_, _| Ok(()))),
                    );
                    Ok(())
                })),
                &mut token,
            );
        }
    }

    fn remove_document_created_script(core_webview: &ICoreWebView2, id: &str) {
        let id_hstr = HSTRING::from(id);
        // SAFETY: Plain COM call on a live `ICoreWebView2`; unknown ids are ignored by WebView2.
        unsafe {
            let _ = core_webview.RemoveScriptToExecuteOnDocumentCreated(&id_hstr);
        }
    }

    /// Creates an in-memory COM stream holding a copy of `bytes` via `SHCreateMemStream`.
    ///
    /// The WebView2 bindings take `windows::Win32::System::Com::IStream`, but the `windows` crate
    /// is not a direct dependency. `_type_witness` (never called) pins `T` to exactly the stream
    /// type returned by `ICoreWebView2WebResourceResponse::Content`, i.e. the type
    /// `CreateWebResourceResponse` expects.
    fn memory_stream_from_bytes<T, F>(_type_witness: F, bytes: &[u8]) -> Option<T>
    where
        T: Interface,
        F: FnOnce(&ICoreWebView2WebResourceResponse) -> windows_core::Result<T>,
    {
        let len = u32::try_from(bytes.len()).ok()?;
        let data = if bytes.is_empty() {
            ptr::null()
        } else {
            bytes.as_ptr()
        };
        // SAFETY: `data` points to `len` readable bytes (or is null with `len == 0`);
        // `SHCreateMemStream` copies them into a new stream.
        let raw = unsafe { SHCreateMemStream(data, len) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `SHCreateMemStream` returns a new `IStream` reference that we now own; `T` is
        // the `IStream` interface type (see `_type_witness`).
        Some(unsafe { T::from_raw(raw) })
    }

    pub fn attach_webview_adblocker<F>(
        webview: &wry::WebView,
        tab_id: usize,
        adblocker: Adblocker,
        on_blocked: F,
    ) where
        F: Fn() + Send + Sync + 'static,
    {
        let controller = webview.controller();
        // SAFETY: `controller` is a live COM pointer owned by `webview` on the main UI thread.
        let Ok(core_webview) = (unsafe { controller.CoreWebView2() }) else {
            return;
        };
        let Ok(core_webview2) = core_webview.cast::<ICoreWebView2_2>() else {
            return;
        };
        let Ok(env) = (unsafe { core_webview2.Environment() }) else {
            return;
        };

        // 1. Register cosmetic & scriptlet bootstrap script via `AddScriptToExecuteOnDocumentCreated`
        // (runs on every document & iframe at document-start).
        let script_hstring = HSTRING::from(COSMETIC_DOCUMENT_CREATED_SCRIPT);
        let completion_handler =
            AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(())));
        // SAFETY: Invokes `AddScriptToExecuteOnDocumentCreated` asynchronously without blocking the UI loop.
        unsafe {
            let _ = core_webview
                .AddScriptToExecuteOnDocumentCreated(&script_hstring, &completion_handler);
        }

        let doc_state = Arc::new(Mutex::new(TopLevelDocState::default()));
        let preload_slot = Arc::new(Mutex::new(PreloadScriptSlot::default()));

        // 2. Hook top-level `NavigationStarting` so `injected_script` & initial cosmetic selectors
        // are preloaded at document-start, and so the upcoming main-frame document request can be
        // told apart from iframe documents. Iframe navigations (`FrameNavigationStarting`) must
        // NOT replace the page's preload script: that would leak one site's selectors/scriptlets
        // into later documents. Iframes rely on `COSMETIC_DOCUMENT_CREATED_SCRIPT`.
        {
            let adblocker_nav = adblocker.clone();
            let doc_state_nav = Arc::clone(&doc_state);
            let script_slot_nav = Arc::clone(&preload_slot);
            let mut nav_token: i64 = 0;
            // SAFETY: Registers `NavigationStarting` COM callback for the lifetime of `core_webview`.
            unsafe {
                let _ = core_webview.add_NavigationStarting(
                    &NavigationStartingEventHandler::create(Box::new(move |sender, args| {
                        let Some(args) = args else {
                            return Ok(());
                        };
                        let mut uri_pwstr = PWSTR::null();
                        if args.Uri(&mut uri_pwstr).is_ok() {
                            let uri = take_pwstr(uri_pwstr);
                            if let Ok(mut guard) = doc_state_nav.lock() {
                                guard.begin_navigation(&uri);
                            }
                            if uri.starts_with("http://") || uri.starts_with("https://") {
                                adblocker_nav.reset_tab_debug(tab_id, &uri);
                            }
                            if let Some(cw) = sender {
                                update_dynamic_preload_script(
                                    &cw,
                                    &script_slot_nav,
                                    &adblocker_nav,
                                    tab_id,
                                    &uri,
                                );
                            }
                        }
                        Ok(())
                    })),
                    &mut nav_token,
                );
            }
        }

        // 2b. Per-iframe cosmetic channel: iframe web messages are only delivered through
        // `ICoreWebView2Frame2::WebMessageReceived`, and rules must be applied with that frame's
        // `ExecuteScript` (the tab-level `evaluate_script` would target the top document).
        // Note: `ICoreWebView2_4::FrameCreated` reports only direct child frames of the page.
        if let Ok(core_webview4) = core_webview.cast::<ICoreWebView2_4>() {
            let adblocker_frames = adblocker.clone();
            let doc_state_frames = Arc::clone(&doc_state);
            let mut frame_token: i64 = 0;
            // SAFETY: Registers the `FrameCreated` COM callback for the lifetime of `core_webview`.
            unsafe {
                let _ = core_webview4.add_FrameCreated(
                    &FrameCreatedEventHandler::create(Box::new(move |_sender, args| {
                        if let Some(frame) = args.and_then(|a| a.Frame().ok()) {
                            attach_frame_cosmetic_handler(
                                &frame,
                                tab_id,
                                adblocker_frames.clone(),
                                Arc::clone(&doc_state_frames),
                            );
                        }
                        Ok(())
                    })),
                    &mut frame_token,
                );
            }
        }

        // 3. Register `AddWebResourceRequestedFilterWithRequestSourceKinds` (covering pages + workers)
        // when `ICoreWebView2_22` is available, falling back to `AddWebResourceRequestedFilter`.
        let filter = HSTRING::from("*");
        if let Ok(core_webview22) = core_webview.cast::<ICoreWebView2_22>() {
            // SAFETY: Registers wildcard interception for both Document and Worker request source kinds.
            unsafe {
                let _ = core_webview22.AddWebResourceRequestedFilterWithRequestSourceKinds(
                    &filter,
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
                );
            }
        } else {
            // SAFETY: Fallback wildcard resource interception filter on older WebView2 runtimes.
            unsafe {
                let _ = core_webview
                    .AddWebResourceRequestedFilter(&filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL);
            }
        }

        // 4. Register fast, non-blocking `WebResourceRequested` handler
        let mut token: i64 = 0;
        let status_phrase = HSTRING::from("No Content");
        let ok_phrase = HSTRING::from("OK");
        let cors_headers = HSTRING::from("Access-Control-Allow-Origin: *");
        let hstr_dnt = HSTRING::from("DNT");
        let hstr_gpc = HSTRING::from("Sec-GPC");
        let hstr_one = HSTRING::from("1");
        let hstr_cookie = HSTRING::from("Cookie");

        // SAFETY: Registers the COM event handler for `WebResourceRequested` for the lifetime of `core_webview`.
        unsafe {
            let _ = core_webview.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |sender, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };

                    let request = args.Request()?;
                    let request_url = {
                        let mut uri_pwstr = PWSTR::null();
                        request.Uri(&mut uri_pwstr)?;
                        take_pwstr(uri_pwstr)
                    };

                    let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT::default();
                    let _ = args.ResourceContext(&mut context);

                    let classification = {
                        let fallback_top = || {
                            let mut src_pwstr = PWSTR::null();
                            match sender.as_ref() {
                                Some(webview_sender)
                                    if webview_sender.Source(&mut src_pwstr).is_ok() =>
                                {
                                    take_pwstr(src_pwstr)
                                }
                                _ => String::new(),
                            }
                        };
                        match doc_state.lock() {
                            Ok(mut state) => {
                                let class = classify_web_request(
                                    context.0,
                                    &request_url,
                                    &state,
                                    fallback_top,
                                );
                                if class.is_main_document {
                                    state.commit(&request_url);
                                }
                                class
                            }
                            Err(_) => classify_web_request(
                                context.0,
                                &request_url,
                                &TopLevelDocState::default(),
                                fallback_top,
                            ),
                        }
                    };
                    let request_type = classification.request_type;
                    let source_url = classification.source_url;

                    let outcome = adblocker.check_network_request_detailed(
                        &request_url,
                        &source_url,
                        request_type,
                    );
                    if outcome.should_block {
                        let rule_label = outcome
                            .matched_rule
                            .clone()
                            .unwrap_or_else(|| String::from("(matched network rule)"));
                        if let Some(redirect_data) = outcome.redirect.as_deref() {
                            // adblock-rust hands us the replacement resource as a `data:` URL;
                            // serve its decoded body (an empty body breaks pages that expect
                            // the stub script / pixel to exist).
                            let (mime, body) = decode_data_url(redirect_data)
                                .unwrap_or_else(|| (String::from("text/plain"), Vec::new()));
                            let headers = HSTRING::from(redirect_response_headers(&mime));
                            let stream = memory_stream_from_bytes(
                                |resp: &ICoreWebView2WebResourceResponse| resp.Content(),
                                &body,
                            );
                            let response = match stream.as_ref() {
                                Some(stream) => env.CreateWebResourceResponse(
                                    Some(stream),
                                    200,
                                    &ok_phrase,
                                    &headers,
                                ),
                                None => {
                                    env.CreateWebResourceResponse(None, 200, &ok_phrase, &headers)
                                }
                            };
                            if let Ok(redirect_resp) = response {
                                let _ = args.SetResponse(&redirect_resp);
                                adblocker.record_debug_event(
                                    tab_id,
                                    AdblockMatchKind::NetworkRedirected,
                                    rule_label,
                                    request_url,
                                    request_type.to_owned(),
                                );
                                on_blocked();
                            }
                        } else if let Ok(blocked_response) =
                            env.CreateWebResourceResponse(None, 204, &status_phrase, &cors_headers)
                        {
                            let _ = args.SetResponse(&blocked_response);
                            adblocker.record_debug_event(
                                tab_id,
                                AdblockMatchKind::NetworkBlocked,
                                rule_label,
                                request_url,
                                request_type.to_owned(),
                            );
                            on_blocked();
                        }
                    } else {
                        // Privacy signals / cookie policy on requests that are allowed through.
                        // Third-party iframe documents count too: their source is the top-level
                        // page (main-frame documents use their own URL, so are never 3rd-party).
                        let send_signals = adblocker.send_dnt_gpc();
                        let strip_cookies = adblocker.block_third_party_cookies()
                            && crate::privacy::is_third_party(&request_url, &source_url);
                        if send_signals || strip_cookies {
                            if let Ok(headers) = request.Headers() {
                                if send_signals {
                                    let _ = headers.SetHeader(&hstr_dnt, &hstr_one);
                                    let _ = headers.SetHeader(&hstr_gpc, &hstr_one);
                                }
                                if strip_cookies {
                                    let _ = headers.RemoveHeader(&hstr_cookie);
                                }
                            }
                        }
                    }

                    Ok(())
                })),
                &mut token,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use std::path::Path;

    use super::Adblocker;

    pub fn download_url_to_file(_url: &str, _dest: &Path) -> bool {
        false
    }

    pub fn measure_process_tree_ram_mb() -> f64 {
        0.0
    }

    pub fn attach_webview_adblocker<F>(
        _webview: &wry::WebView,
        _tab_id: usize,
        _adblocker: Adblocker,
        _on_blocked: F,
    ) where
        F: Fn() + Send + Sync + 'static,
    {
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn test_resource_context_mapping() {
        assert_eq!(map_webview2_resource_context(1, false), "document");
        assert_eq!(map_webview2_resource_context(1, true), "subdocument");
        assert_eq!(map_webview2_resource_context(2, false), "stylesheet");
        assert_eq!(map_webview2_resource_context(3, false), "image");
        assert_eq!(map_webview2_resource_context(4, false), "media");
        assert_eq!(map_webview2_resource_context(5, false), "font");
        assert_eq!(map_webview2_resource_context(6, false), "script");
        assert_eq!(map_webview2_resource_context(7, false), "xmlhttprequest");
        assert_eq!(map_webview2_resource_context(8, false), "fetch");
        assert_eq!(map_webview2_resource_context(11, false), "websocket");
        assert_eq!(map_webview2_resource_context(14, false), "ping");
        assert_eq!(map_webview2_resource_context(15, false), "csp_report");
        assert_eq!(map_webview2_resource_context(16, false), "other");
    }

    #[test]
    fn test_network_matching_and_per_site_allowlist() {
        let blocker = Adblocker::new();

        // Third-party ad script on a news site should be blocked
        assert!(blocker.check_network_request(
            "https://securepubads.g.doubleclick.net/gpt/pubads_impl_2026.js",
            "https://www.cnn.com/world",
            "script",
        ));

        // Third-party tracker pixel should be blocked
        assert!(blocker.check_network_request(
            "https://www.google-analytics.com/collect?v=1",
            "https://www.bbc.com/news",
            "xmlhttprequest",
        ));

        // First-party article stylesheet/image should NOT be blocked
        assert!(!blocker.check_network_request(
            "https://www.cnn.com/css/main.css",
            "https://www.cnn.com/world",
            "stylesheet",
        ));

        // Per-site allowlist toggle should bypass blocking for cnn.com while keeping bbc.com protected
        assert_eq!(
            blocker.toggle_site_allowlist("https://www.cnn.com/world"),
            Some(true)
        );
        assert!(blocker.is_site_allowlisted("https://cnn.com/politics"));
        assert!(!blocker.check_network_request(
            "https://securepubads.g.doubleclick.net/gpt/pubads_impl_2026.js",
            "https://www.cnn.com/world",
            "script",
        ));
        assert!(blocker.check_network_request(
            "https://www.google-analytics.com/collect?v=1",
            "https://www.bbc.com/news",
            "xmlhttprequest",
        ));

        // Toggling cnn.com again re-enables blocking
        assert_eq!(
            blocker.toggle_site_allowlist("https://cnn.com"),
            Some(false)
        );
        assert!(blocker.check_network_request(
            "https://securepubads.g.doubleclick.net/gpt/pubads_impl_2026.js",
            "https://www.cnn.com/world",
            "script",
        ));
    }

    #[test]
    fn test_engine_serialization_and_cosmetic_filtering() {
        let rules = &[
            "||ads.example.org^$third-party",
            "news.example.com##.sponsored-banner",
            "##.generic-ad-box",
        ];
        let blocker = Adblocker::from_rules(rules);

        let serialized = blocker.serialize_engine();
        assert!(serialized.is_some());
        let bytes = serialized.unwrap_or_default();
        assert!(!bytes.is_empty());

        let restored = Adblocker::from_rules(&[]);
        assert!(restored.deserialize_engine(&bytes));

        assert!(restored.check_network_request(
            "https://ads.example.org/banner.js",
            "https://news.example.com/article",
            "script",
        ));

        let cosmetic = restored.cosmetic_resources_for_url("https://news.example.com/article");
        assert!(cosmetic
            .hide_selectors
            .iter()
            .any(|s| s.contains(".sponsored-banner")));
        assert!(!cosmetic.generichide);

        let generic = restored.hidden_class_id_selectors(
            &[String::from("generic-ad-box"), String::from("article-body")],
            &[],
            &cosmetic.exceptions,
        );
        assert!(generic.iter().any(|s| s.contains(".generic-ad-box")));
    }

    #[test]
    fn benchmark_request_match_latency() {
        let blocker = Adblocker::new();
        let sample_requests = [
            (
                "https://www.cnn.com/assets/app.bundle.js",
                "https://www.cnn.com/",
                "script",
            ),
            (
                "https://securepubads.g.doubleclick.net/gpt/pubads_impl.js",
                "https://www.cnn.com/",
                "script",
            ),
            (
                "https://www.nytimes.com/images/header.webp",
                "https://www.nytimes.com/",
                "image",
            ),
            (
                "https://ib.adnxs.com/ut/v3/prebid",
                "https://www.nytimes.com/",
                "xmlhttprequest",
            ),
            (
                "https://www.theguardian.com/stylesheets/main.css",
                "https://www.theguardian.com/",
                "stylesheet",
            ),
        ];

        let iterations: u32 = 10_000;
        let start = Instant::now();
        let mut blocked = 0usize;
        for i in 0..iterations {
            let (req_url, src_url, req_type) =
                sample_requests[(i as usize) % sample_requests.len()];
            if blocker.check_network_request(req_url, src_url, req_type) {
                blocked += 1;
            }
        }
        let elapsed = start.elapsed();
        let avg_ns = elapsed.as_nanos() / u128::from(iterations);
        println!(
            "Adblock match benchmark: {} requests in {:?} ({} ns / {:.2} Âµs per request, {} blocked)",
            iterations,
            elapsed,
            avg_ns,
            (avg_ns as f64) / 1000.0,
            blocked
        );
        assert_eq!(blocked, 4_000);
        // Ensure per-request match time is well under 500 Âµs even in unoptimized debug builds
        assert!(avg_ns < 500_000);
    }

    #[test]
    fn test_youtube_scriptlet_injection_and_preload() {
        let blocker = Adblocker::from_rules(&[
            "youtube.com##+js(json-prune, playerResponse.adPlacements adPlacements)",
            "youtube.com##+js(set-constant, ytInitialPlayerResponse.adPlacements, undefined)",
            "youtube.com##+js(trusted-replace-xhr-response, /\"adPlacements\":\\s*\\[[^\\]]*\\]/, \"\", /youtubei\\/v1\\/player/)",
            "youtube.com##ytd-ad-slot-renderer",
        ]);

        let resources =
            blocker.cosmetic_resources_for_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert!(resources
            .hide_selectors
            .iter()
            .any(|s| s == "ytd-ad-slot-renderer"));
        assert!(resources.injected_script.contains("emberJsonPrune"));
        assert!(resources.injected_script.contains("emberSetConstant"));
        assert!(resources
            .injected_script
            .contains("emberTrustedReplaceXhrResponse"));

        let preload = build_document_start_preload_script(&resources);
        assert!(preload.contains("window.__emberPreloadedCosmetic="));
        assert!(preload.contains("emberJsonPrune"));
        assert!(preload.contains("window.__emberApplyPreloadedCosmetic"));
    }

    // ---- decode_data_url -------------------------------------------------------------------

    #[test]
    fn test_decode_data_url_base64() {
        // "(function(){})();" as produced by adblock-rust for `noop.js`.
        let (mime, body) =
            decode_data_url("data:application/javascript;base64,KGZ1bmN0aW9uKCl7fSkoKTs=")
                .unwrap_or_default();
        assert_eq!(mime, "application/javascript");
        assert_eq!(body, b"(function(){})();");

        // 1x1 transparent GIF header bytes, unpadded input and embedded whitespace.
        let (mime, body) = decode_data_url("data:image/gif;base64,R0lG\nODlh").unwrap_or_default();
        assert_eq!(mime, "image/gif");
        assert_eq!(body, b"GIF89a");

        let (_, body) = decode_data_url("data:text/plain;base64,YQ").unwrap_or_default();
        assert_eq!(body, b"a");
        let (_, body) = decode_data_url("data:text/plain;base64,YWI=").unwrap_or_default();
        assert_eq!(body, b"ab");
        let (_, body) =
            decode_data_url("DATA:Text/Plain;charset=utf-8;BASE64,").unwrap_or_default();
        assert!(body.is_empty());
    }

    #[test]
    fn test_decode_data_url_percent_encoded_and_plain() {
        let (mime, body) =
            decode_data_url("data:text/css,%2Fbody%7Bcolor%3Ared%7D%20").unwrap_or_default();
        assert_eq!(mime, "text/css");
        assert_eq!(body, b"/body{color:red} ");

        let (mime, body) = decode_data_url("data:text/html,<p>hi</p>").unwrap_or_default();
        assert_eq!(mime, "text/html");
        assert_eq!(body, b"<p>hi</p>");

        // Base64 payload that is itself percent-encoded.
        let (_, body) = decode_data_url("data:text/plain;base64,YWI%3D").unwrap_or_default();
        assert_eq!(body, b"ab");

        // Missing / malformed MIME falls back to text/plain; stray `%` is kept literally.
        let (mime, body) = decode_data_url("data:,100%").unwrap_or_default();
        assert_eq!(mime, "text/plain");
        assert_eq!(body, b"100%");
        let (mime, _) = decode_data_url("data:bad\r\nInjected: 1,x").unwrap_or_default();
        assert_eq!(mime, "text/plain");
    }

    #[test]
    fn test_decode_data_url_invalid() {
        assert!(decode_data_url("https://example.com/a.js").is_none());
        assert!(decode_data_url("data:text/plain").is_none());
        assert!(decode_data_url("data").is_none());
        assert!(decode_data_url("").is_none());
        assert!(decode_data_url("data:text/plain;base64,@@@@").is_none());
        assert!(decode_data_url("data:text/plain;base64,YWJjZ").is_none());
        assert!(decode_data_url("data:text/plain;base64,YW=I").is_none());
    }

    #[test]
    fn test_redirect_response_headers() {
        assert_eq!(
            redirect_response_headers("image/gif"),
            "Content-Type: image/gif\r\nAccess-Control-Allow-Origin: *"
        );
        assert_eq!(
            redirect_response_headers("text/x\r\nSet-Cookie: a=b"),
            "Content-Type: text/plain\r\nAccess-Control-Allow-Origin: *"
        );
    }

    #[test]
    fn test_redirect_resources_decode() {
        // Every `$redirect` the engine hands back must decode to a non-empty body.
        let blocker = Adblocker::from_rules(&[
            "||ads.example.org/ad.js$script,redirect=noopjs",
            "||ads.example.org/px.gif$image,redirect=1x1.gif",
        ]);
        for (url, ty) in [
            ("https://ads.example.org/ad.js", "script"),
            ("https://ads.example.org/px.gif", "image"),
        ] {
            let outcome =
                blocker.check_network_request_detailed(url, "https://news.example.com/", ty);
            assert!(outcome.redirect.is_some(), "no redirect for {url}");
            let redirect = outcome.redirect.unwrap_or_default();
            let decoded = decode_data_url(&redirect);
            assert!(decoded.is_some(), "undecodable redirect {redirect}");
            let (mime, body) = decoded.unwrap_or_default();
            assert!(!mime.is_empty());
            assert!(!body.is_empty(), "empty body for {url}");
        }
    }

    // ---- request classification ------------------------------------------------------------

    fn no_fallback() -> String {
        String::new()
    }

    #[test]
    fn test_classify_top_level_navigation_while_old_page_is_source() {
        // NavigationStarting fired for b.com, WebView2 Source still reports a.com.
        let mut state = TopLevelDocState::default();
        state.commit("https://a.com/page");
        state.begin_navigation("https://b.com/next#frag");

        let class = classify_web_request(1, "https://b.com/next", &state, || {
            String::from("https://a.com/page")
        });
        assert_eq!(class.request_type, "document");
        assert_eq!(class.source_url, "https://b.com/next");
        assert!(class.is_main_document);

        state.commit("https://b.com/next");
        assert_eq!(state.pending_nav_url, None);
        // Subresources of the new page use it as their source.
        let sub = classify_web_request(6, "https://cdn.c.com/x.js", &state, no_fallback);
        assert_eq!(sub.request_type, "script");
        assert_eq!(sub.source_url, "https://b.com/next");
        assert!(!sub.is_main_document);
    }

    #[test]
    fn test_classify_iframe_document() {
        let mut state = TopLevelDocState::default();
        state.commit("https://news.example.com/");
        let class = classify_web_request(1, "https://ads.adnet.io/frame.html", &state, no_fallback);
        assert_eq!(class.request_type, "subdocument");
        assert_eq!(class.source_url, "https://news.example.com/");
        assert!(!class.is_main_document);

        // An iframe loading while a top-level navigation is pending still belongs to the old page.
        state.begin_navigation("https://other.com/");
        let class = classify_web_request(1, "https://ads.adnet.io/frame.html", &state, no_fallback);
        assert_eq!(class.request_type, "subdocument");
        assert_eq!(class.source_url, "https://news.example.com/");
    }

    #[test]
    fn test_classify_reload_and_first_navigation() {
        // Reload of the current page (no pending navigation recorded).
        let mut state = TopLevelDocState::default();
        state.commit("https://a.com/");
        let class = classify_web_request(1, "https://a.com/", &state, no_fallback);
        assert!(class.is_main_document);
        assert_eq!(class.request_type, "document");

        // Handlers attached after the first NavigationStarting: no state, Source is about:blank.
        let empty = TopLevelDocState::default();
        let class = classify_web_request(1, "https://first.com/", &empty, || {
            String::from("about:blank")
        });
        assert!(class.is_main_document);
        assert_eq!(class.source_url, "https://first.com/");

        // No state: subresources fall back to the WebView source.
        let class = classify_web_request(3, "https://img.com/a.png", &empty, || {
            String::from("https://first.com/")
        });
        assert_eq!(class.request_type, "image");
        assert_eq!(class.source_url, "https://first.com/");
        let class = classify_web_request(3, "https://img.com/a.png", &empty, || {
            String::from("about:blank")
        });
        assert_eq!(class.source_url, "");
    }

    #[test]
    fn test_classify_does_not_call_fallback_when_state_known() {
        let mut state = TopLevelDocState::default();
        state.commit("https://a.com/");
        let class = classify_web_request(8, "https://api.a.com/x", &state, || {
            panic!("fallback must not be used when the top-level URL is known")
        });
        assert_eq!(class.request_type, "fetch");
        assert_eq!(class.source_url, "https://a.com/");
    }

    // ---- preload script generations ----------------------------------------------------------

    #[test]
    fn test_preload_slot_in_order_completion() {
        let mut slot = PreloadScriptSlot::default();
        let (g1, prev) = slot.begin_update();
        assert_eq!(prev, None);
        assert_eq!(slot.complete(g1, String::from("id1")), None);
        assert_eq!(slot.current_id(), Some("id1"));

        let (g2, prev) = slot.begin_update();
        assert_eq!(prev.as_deref(), Some("id1"));
        assert_eq!(slot.complete(g2, String::from("id2")), None);
        assert_eq!(slot.current_id(), Some("id2"));
    }

    #[test]
    fn test_preload_slot_stale_completion_removes_itself() {
        let mut slot = PreloadScriptSlot::default();
        // Two navigations start before the first registration completes.
        let (g1, prev1) = slot.begin_update();
        let (g2, prev2) = slot.begin_update();
        assert_eq!((prev1, prev2), (None, None));

        // Late completion of the first registration: must be removed, not stored.
        assert_eq!(
            slot.complete(g1, String::from("id1")).as_deref(),
            Some("id1")
        );
        assert_eq!(slot.current_id(), None);

        assert_eq!(slot.complete(g2, String::from("id2")), None);
        assert_eq!(slot.current_id(), Some("id2"));

        // Completions arriving out of order: newer first, then the stale one.
        let (g3, prev3) = slot.begin_update();
        assert_eq!(prev3.as_deref(), Some("id2"));
        let (g4, _) = slot.begin_update();
        assert_eq!(slot.complete(g4, String::from("id4")), None);
        assert_eq!(
            slot.complete(g3, String::from("id3")).as_deref(),
            Some("id3")
        );
        assert_eq!(slot.current_id(), Some("id4"));
    }

    // ---- filter-list rebuild planning --------------------------------------------------------

    #[test]
    fn test_next_rebuild_action_uses_current_lists() {
        assert_eq!(
            next_rebuild_action((true, true), (true, true), false),
            RebuildNext::Done
        );
        // Lists changed mid-build: the next pass must target the NEW selection.
        assert_eq!(
            next_rebuild_action((true, true), (false, true), false),
            RebuildNext::Rerun {
                force_download: false,
                lists: (false, true)
            }
        );
        // `update_now()` during an update forces another downloading pass.
        assert_eq!(
            next_rebuild_action((true, true), (true, true), true),
            RebuildNext::Rerun {
                force_download: true,
                lists: (true, true)
            }
        );
    }

    #[test]
    fn test_cache_fallback_and_list_encoding() {
        for lists in [
            None,
            Some((false, false)),
            Some((true, false)),
            Some((false, true)),
            Some((true, true)),
        ] {
            assert_eq!(decode_engine_lists(encode_engine_lists(lists)), lists);
        }
        // Offline toggle: active engine built for other lists -> recompile from cache.
        assert!(needs_cache_compile(Some((true, true)), (true, false)));
        // Bootstrap-only engine -> recompile from cache.
        assert!(needs_cache_compile(None, (true, true)));
        // Offline periodic update with a matching engine -> nothing to do.
        assert!(!needs_cache_compile(Some((true, true)), (true, true)));
    }

    #[test]
    fn test_all_requested_succeeded() {
        assert!(all_requested_succeeded(&[
            (true, true),
            (false, false),
            (true, true)
        ]));
        assert!(!all_requested_succeeded(&[(true, true), (true, false)]));
        assert!(all_requested_succeeded(&[]));
    }

    #[test]
    fn test_refresh_plan_for_new_list_selection() {
        let dir = std::env::temp_dir().join(format!(
            "ember-adblock-plan-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        assert!(fs::create_dir_all(&dir).is_ok());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string();
        for file in [
            EASYLIST_CACHE_FILE,
            UBLOCK_FILTERS_CACHE_FILE,
            UBLOCK_UNBREAK_CACHE_FILE,
            UBLOCK_QUICK_FIXES_CACHE_FILE,
            UBLOCK_ANNOYANCES_CACHE_FILE,
            engine_cache_name(true, false),
        ] {
            assert!(fs::write(dir.join(file), "x").is_ok());
        }
        assert!(fs::write(dir.join(LAST_UPDATED_FILE), &now).is_ok());
        assert!(fs::write(dir.join(LAST_QUICK_FIXES_UPDATED_FILE), &now).is_ok());

        // Everything cached and fresh for EasyList-only.
        assert_eq!(
            refresh_plan_for_dir(&dir, true, false),
            RefreshPlan::default()
        );
        // Switching to EasyPrivacy (not cached, no engine snapshot) needs downloads.
        let plan = refresh_plan_for_dir(&dir, false, true);
        assert!(plan.standard);
        // Disabling both lists with a cached snapshot for that selection needs no download.
        assert!(fs::write(dir.join(engine_cache_name(false, false)), "x").is_ok());
        assert_eq!(
            refresh_plan_for_dir(&dir, false, false),
            RefreshPlan::default()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_preload_script_is_gated_to_top_frame() {
        let resources = CosmeticResources {
            hide_selectors: vec![String::from(".top-ad")],
            injected_script: String::from("window.__topOnly=1;"),
            ..CosmeticResources::default()
        };
        let preload = build_document_start_preload_script(&resources);
        assert!(preload.starts_with("if(window===window.top){"));
        assert!(preload.ends_with('}'));
        // Balanced braces: the whole assignment + apply call sits inside the gate.
        assert_eq!(preload.matches('{').count(), preload.matches('}').count());
        // The document-created script applies preloaded rules only in the top frame and posts
        // iframe messages through the per-frame `chrome.webview` channel.
        assert!(
            COSMETIC_DOCUMENT_CREATED_SCRIPT.contains("var isTopFrame = window === window.top;")
        );
        assert!(COSMETIC_DOCUMENT_CREATED_SCRIPT
            .contains("if (isTopFrame && window.__emberPreloadedCosmetic)"));
        assert!(COSMETIC_DOCUMENT_CREATED_SCRIPT
            .contains("if (!isTopFrame && window.chrome && window.chrome.webview"));
    }

    #[test]
    fn test_parse_frame_cosmetic_message() {
        use crate::ui::PageIpcEvent;
        assert_eq!(
            parse_frame_cosmetic_message("cosmetic_init:https://ads.example.net/frame"),
            Some(PageIpcEvent::CosmeticInit(String::from(
                "https://ads.example.net/frame"
            )))
        );
        assert_eq!(
            parse_frame_cosmetic_message(
                r#"cosmetic_tokens:{"url":"https://ads.example.net/","classes":["ad-box"],"ids":["banner"]}"#
            ),
            Some(PageIpcEvent::CosmeticTokens {
                url: String::from("https://ads.example.net/"),
                classes: vec![String::from("ad-box")],
                ids: vec![String::from("banner")],
            })
        );
        assert!(matches!(
            parse_frame_cosmetic_message(r#"scriptlet_log:{"url":"https://a.b/","name":"x"}"#),
            Some(PageIpcEvent::ScriptletLog { .. })
        ));
        // Non-cosmetic page IPC (shortcuts, favicon, ...) is ignored from iframes.
        assert_eq!(parse_frame_cosmetic_message("shortcut:close_tab"), None);
        assert_eq!(parse_frame_cosmetic_message("first_paint"), None);
        assert_eq!(parse_frame_cosmetic_message("cosmetic_init:"), None);
        assert_eq!(parse_frame_cosmetic_message("garbage"), None);
    }

    #[test]
    fn test_frame_cosmetic_script_uses_frame_url_and_records_debug() {
        use crate::ui::PageIpcEvent;
        let blocker = Adblocker::from_rules(&[
            "ads.example.net##.frame-ad",
            "news.example.com##.top-ad",
            "##.generic-ad",
        ]);
        let tab_id = 7;
        let top = "https://news.example.com/article";
        let init = PageIpcEvent::CosmeticInit(String::from("https://ads.example.net/frame"));
        let script = frame_cosmetic_script(&blocker, tab_id, top, &init).unwrap_or_default();
        assert!(script.contains(".frame-ad"));
        assert!(!script.contains(".top-ad"));
        let debug = blocker.tab_debug_snapshot(tab_id);
        assert!(debug.entries.iter().any(|e| e.rule_or_name == ".frame-ad"
            && e.request_type == "iframe-cosmetic"
            && e.kind == AdblockMatchKind::CosmeticSelector));

        let tokens = PageIpcEvent::CosmeticTokens {
            url: String::from("https://ads.example.net/frame"),
            classes: vec![String::from("generic-ad"), String::from("content")],
            ids: Vec::new(),
        };
        let script = frame_cosmetic_script(&blocker, tab_id, top, &tokens).unwrap_or_default();
        assert!(script.contains(".generic-ad"));
        let debug = blocker.tab_debug_snapshot(tab_id);
        assert!(debug.entries.iter().any(
            |e| e.rule_or_name == ".generic-ad" && e.request_type == "iframe-generic-cosmetic"
        ));

        // Tokens that match nothing produce no script.
        let none = PageIpcEvent::CosmeticTokens {
            url: String::from("https://ads.example.net/frame"),
            classes: vec![String::from("content")],
            ids: Vec::new(),
        };
        assert_eq!(frame_cosmetic_script(&blocker, tab_id, top, &none), None);

        // Non-web frames (about:blank, data:) are skipped.
        let blank = PageIpcEvent::CosmeticInit(String::from("about:blank"));
        assert_eq!(frame_cosmetic_script(&blocker, tab_id, top, &blank), None);
    }

    #[test]
    fn test_frame_cosmetic_script_respects_top_page_allowlist() {
        use crate::ui::PageIpcEvent;
        let blocker = Adblocker::from_rules(&["ads.example.net##.frame-ad", "##.generic-ad"]);
        assert!(blocker.set_site_allowlisted("news.example.com", true));
        let top = "https://news.example.com/";
        let init = PageIpcEvent::CosmeticInit(String::from("https://ads.example.net/frame"));
        let script = frame_cosmetic_script(&blocker, 1, top, &init).unwrap_or_default();
        assert!(!script.contains(".frame-ad"));
        // generichide=true so the frame stops reporting tokens.
        assert!(script.contains("],true,\""));
        let tokens = PageIpcEvent::CosmeticTokens {
            url: String::from("https://ads.example.net/frame"),
            classes: vec![String::from("generic-ad")],
            ids: Vec::new(),
        };
        assert_eq!(frame_cosmetic_script(&blocker, 1, top, &tokens), None);
    }

    #[test]
    fn test_cosmetic_message_matches_source() {
        assert!(cosmetic_message_matches_source(
            "https://www.example.com/a?b#c",
            "https://example.com/other"
        ));
        assert!(!cosmetic_message_matches_source(
            "https://ads.example.net/frame",
            "https://news.example.com/"
        ));
        // Unknown / non-web sender: accepted.
        assert!(cosmetic_message_matches_source("https://a.com/", ""));
        assert!(cosmetic_message_matches_source(
            "https://a.com/",
            "about:blank"
        ));
        // Web sender but non-web message URL: rejected.
        assert!(!cosmetic_message_matches_source(
            "file:///x",
            "https://a.com/"
        ));
    }

    #[test]
    fn test_ipv6_host_extraction() {
        assert_eq!(
            extract_normalized_host_str("http://[::1]:8080/path"),
            Some("[::1]")
        );
        assert_eq!(
            extract_normalized_host_str("https://[2001:db8::1]/"),
            Some("[2001:db8::1]")
        );
    }
}
