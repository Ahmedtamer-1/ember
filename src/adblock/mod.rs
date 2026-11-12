use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
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

/// Interval (24 hours) between automatic background filter list updates.
pub const FILTER_UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Serialized engine cache filename inside `%LOCALAPPDATA%\Ember\filters`.
const ENGINE_CACHE_FILE: &str = "engine.dat";
const EASYLIST_CACHE_FILE: &str = "easylist.txt";
const EASYPRIVACY_CACHE_FILE: &str = "easyprivacy.txt";
const LAST_UPDATED_FILE: &str = "last_updated.txt";
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

  function postIpc(msg) {
    if (window.ipc && typeof window.ipc.postMessage === "function") {
      window.ipc.postMessage(msg);
    } else if (window.chrome && window.chrome.webview && typeof window.chrome.webview.postMessage === "function") {
      window.chrome.webview.postMessage(msg);
    }
  }

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
      try {
        var fn = new Function(scriptlet);
        fn();
      } catch (_) {}
    }
  };

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

  function startObserver() {
    if (location.protocol === "http:" || location.protocol === "https:") {
      postIpc("cosmetic_init:" + location.href);
    }
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
    send_dnt_gpc: Arc<AtomicBool>,
    block_third_party_cookies: Arc<AtomicBool>,
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
    ) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            engine: Arc::new(RwLock::new(Arc::new(engine))),
            allowlist: Arc::new(RwLock::new(allowlist)),
            filters_dir,
            easylist_enabled: Arc::new(AtomicBool::new(easylist)),
            easyprivacy_enabled: Arc::new(AtomicBool::new(easyprivacy)),
            updating: Arc::new(AtomicBool::new(false)),
            send_dnt_gpc: Arc::new(AtomicBool::new(false)),
            block_third_party_cookies: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Creates a new active adblocker initialized with the embedded bootstrap filter rules.
    pub fn new() -> Self {
        let engine = compile_engine_from_rules(DEFAULT_BOOTSTRAP_RULES.iter().copied());
        Self::assemble(engine, HashSet::new(), None, true, true)
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
        let initial_engine = load_or_bootstrap_engine(filters_dir, easylist, easyprivacy);

        let blocker = Self::assemble(
            initial_engine,
            allowlist_set,
            Some(filters_dir.to_path_buf()),
            easylist,
            easyprivacy,
        );

        blocker.spawn_background_updater();
        blocker
    }

    /// Creates an adblocker from custom filter rule strings (used for tests and benchmarks).
    pub fn from_rules(rules: &[&str]) -> Self {
        let engine = compile_engine_from_rules(rules.iter().copied());
        Self::assemble(engine, HashSet::new(), None, true, true)
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

    /// Fast, non-blocking network request check suitable for WebView2's `WebResourceRequested`.
    /// Performs zero disk or network I/O and holds no locks during engine evaluation.
    pub fn check_network_request(
        &self,
        request_url: &str,
        source_url: &str,
        request_type: &str,
    ) -> bool {
        if !self.is_enabled() {
            return false;
        }

        let req_trimmed = request_url.trim();
        if !(req_trimmed.starts_with("http://")
            || req_trimmed.starts_with("https://")
            || req_trimmed.starts_with("ws://")
            || req_trimmed.starts_with("wss://"))
        {
            return false;
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
                        return false;
                    }
                    if source_host.bytes().any(|b| b.is_ascii_uppercase()) {
                        let lower = source_host.to_ascii_lowercase();
                        if guard.contains(&lower) {
                            return false;
                        }
                    }
                }
            }
        }

        // Clone the `Arc<Engine>` pointer and drop the read lock before running the match.
        let Some(engine) = self.engine.read().ok().map(|guard| Arc::clone(&guard)) else {
            return false;
        };

        let Ok(request) = Request::new(req_trimmed, effective_source, request_type, "get") else {
            return false;
        };

        engine.check_network_request(&request).should_block()
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
                let due = this
                    .filters_dir
                    .as_deref()
                    .is_some_and(|dir| should_refresh_filters(dir, easylist, easyprivacy));
                if due {
                    this.run_rebuild(true);
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
        if self.updating.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut force = force_download;
        loop {
            let (easylist, easyprivacy) = self.lists_enabled();
            let missing = (easylist && !dir.join(EASYLIST_CACHE_FILE).is_file())
                || (easyprivacy && !dir.join(EASYPRIVACY_CACHE_FILE).is_file());
            let download = (force || missing) && (easylist || easyprivacy);
            force = false;

            let engine = if download {
                download_and_compile_filters(&dir, easylist, easyprivacy)
            } else {
                Some(compile_and_persist_engine(&dir, easylist, easyprivacy))
            };

            if self.lists_enabled() != (easylist, easyprivacy) {
                continue;
            }
            if let Some(engine) = engine {
                if let Ok(mut guard) = self.engine.write() {
                    *guard = Arc::new(engine);
                }
            }
            break;
        }
        self.updating.store(false, Ordering::SeqCst);
    }
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
    let host = host_port
        .split_once(':')
        .map_or(host_port, |(h, _)| h)
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

fn compile_engine_from_rules<'a>(rules: impl IntoIterator<Item = &'a str>) -> Engine {
    let mut filter_set = FilterSet::new(false);
    let combined = rules.into_iter().collect::<Vec<_>>().join("\n");
    filter_set.add_filter_list(combined, ParseOptions::default());
    Engine::new_with_filter_set(filter_set)
}

/// Serialized-engine cache file name for a given selection of filter lists.
fn engine_cache_name(easylist: bool, easyprivacy: bool) -> &'static str {
    match (easylist, easyprivacy) {
        (true, true) => ENGINE_CACHE_FILE,
        (true, false) => "engine-easylist.dat",
        (false, true) => "engine-easyprivacy.dat",
        (false, false) => "engine-bootstrap.dat",
    }
}

/// Compiles the embedded bootstrap rules plus whichever enabled lists are cached on disk.
fn compile_engine_from_cache(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> Engine {
    let mut filter_set = FilterSet::new(false);
    filter_set.add_filter_list(DEFAULT_BOOTSTRAP_RULES.join("\n"), ParseOptions::default());
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
    Engine::new_with_filter_set(filter_set)
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

fn load_or_bootstrap_engine(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> Engine {
    let engine_path = filters_dir.join(engine_cache_name(easylist, easyprivacy));
    if let Ok(bytes) = fs::read(&engine_path) {
        if !bytes.is_empty() {
            let mut engine = Engine::default();
            if engine.deserialize(&bytes).is_ok() {
                return engine;
            }
        }
    }

    // No usable snapshot: start with embedded bootstrap rules immediately (<1ms)
    // so cold startup is instant. The background updater will compile/download full lists.
    compile_engine_from_rules(DEFAULT_BOOTSTRAP_RULES.iter().copied())
}

fn should_refresh_filters(filters_dir: &Path, easylist: bool, easyprivacy: bool) -> bool {
    if !easylist && !easyprivacy {
        return false;
    }
    let engine_missing = !filters_dir
        .join(engine_cache_name(easylist, easyprivacy))
        .is_file();
    let easylist_missing = easylist && !filters_dir.join(EASYLIST_CACHE_FILE).is_file();
    let easyprivacy_missing = easyprivacy && !filters_dir.join(EASYPRIVACY_CACHE_FILE).is_file();
    if engine_missing || easylist_missing || easyprivacy_missing {
        return true;
    }

    let last_updated_path = filters_dir.join(LAST_UPDATED_FILE);
    let Ok(contents) = fs::read_to_string(last_updated_path) else {
        return true;
    };
    let Ok(last_epoch) = contents.trim().parse::<u64>() else {
        return true;
    };

    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    now_epoch.saturating_sub(last_epoch) >= FILTER_UPDATE_INTERVAL.as_secs()
}

/// Downloads the enabled lists, compiles + serializes the engine and records the update time.
/// Returns `None` if every requested download failed.
fn download_and_compile_filters(
    filters_dir: &Path,
    easylist: bool,
    easyprivacy: bool,
) -> Option<Engine> {
    let ok_el = easylist
        && platform::download_url_to_file(EASYLIST_URL, &filters_dir.join(EASYLIST_CACHE_FILE));
    let ok_ep = easyprivacy
        && platform::download_url_to_file(
            EASYPRIVACY_URL,
            &filters_dir.join(EASYPRIVACY_CACHE_FILE),
        );
    if !ok_el && !ok_ep {
        return None;
    }

    let engine = compile_and_persist_engine(filters_dir, easylist, easyprivacy);

    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = fs::write(filters_dir.join(LAST_UPDATED_FILE), now_epoch.to_string());

    Some(engine)
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
/// cosmetic filtering to a tab's `wry::WebView`.
pub fn attach_webview_adblocker<F>(webview: &wry::WebView, adblocker: Adblocker, on_blocked: F)
where
    F: Fn() + Send + Sync + 'static,
{
    platform::attach_webview_adblocker(webview, adblocker, on_blocked);
}

#[cfg(target_os = "windows")]
mod platform {
    use core::ffi::c_void;
    use std::{fs, path::Path, ptr};

    use webview2_com::{
        take_pwstr, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_2, COREWEBVIEW2_WEB_RESOURCE_CONTEXT,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        },
        WebResourceRequestedEventHandler,
    };
    use windows_core::{Interface, HSTRING, PWSTR};
    use wry::WebViewExtWindows;

    use super::{map_webview2_resource_context, Adblocker, COSMETIC_DOCUMENT_CREATED_SCRIPT};

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

    pub fn attach_webview_adblocker<F>(webview: &wry::WebView, adblocker: Adblocker, on_blocked: F)
    where
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

        // 1. Register cosmetic filtering bootstrap script via `AddScriptToExecuteOnDocumentCreated`
        let script_hstring = HSTRING::from(COSMETIC_DOCUMENT_CREATED_SCRIPT);
        let completion_handler =
            AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(())));
        // SAFETY: Invokes `AddScriptToExecuteOnDocumentCreated` asynchronously without blocking the UI loop.
        unsafe {
            let _ = core_webview
                .AddScriptToExecuteOnDocumentCreated(&script_hstring, &completion_handler);
        }

        // 2. Register `AddWebResourceRequestedFilter("*", COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)`
        let filter = HSTRING::from("*");
        // SAFETY: Registers the wildcard resource interception filter on the WebView2 instance.
        unsafe {
            let _ = core_webview
                .AddWebResourceRequestedFilter(&filter, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL);
        }

        // 3. Register fast, non-blocking `WebResourceRequested` handler
        let mut token: i64 = 0;
        let status_phrase = HSTRING::from("No Content");
        let cors_headers = HSTRING::from("Access-Control-Allow-Origin: *");
        let hstr_dnt = HSTRING::from("DNT");
        let hstr_gpc = HSTRING::from("Sec-GPC");
        let hstr_one = HSTRING::from("1");
        let hstr_cookie = HSTRING::from("Cookie");
        let current_doc_url = std::sync::Arc::new(std::sync::RwLock::new(String::new()));

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

                    let source_url = if context.0 == 1 {
                        // Document navigation: refresh cached current document URL
                        let webview_source = if let Some(webview_sender) = sender {
                            let mut src_pwstr = PWSTR::null();
                            if webview_sender.Source(&mut src_pwstr).is_ok() {
                                take_pwstr(src_pwstr)
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                        if webview_source.is_empty()
                            || webview_source == "about:blank"
                            || webview_source == request_url
                        {
                            if let Ok(mut guard) = current_doc_url.write() {
                                guard.clone_from(&request_url);
                            }
                        }
                        if !webview_source.is_empty() && webview_source != "about:blank" {
                            webview_source
                        } else {
                            request_url.clone()
                        }
                    } else {
                        // Subresources: read from cached document URL without COM roundtrip
                        let cached = current_doc_url
                            .read()
                            .ok()
                            .map(|g| g.clone())
                            .unwrap_or_default();
                        if !cached.is_empty() && cached != "about:blank" {
                            cached
                        } else if let Some(webview_sender) = sender {
                            let mut src_pwstr = PWSTR::null();
                            if webview_sender.Source(&mut src_pwstr).is_ok() {
                                let src = take_pwstr(src_pwstr);
                                if !src.is_empty() && src != "about:blank" {
                                    if let Ok(mut guard) = current_doc_url.write() {
                                        guard.clone_from(&src);
                                    }
                                }
                                src
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        }
                    };

                    let is_subframe_doc = context.0 == 1
                        && !source_url.is_empty()
                        && source_url != "about:blank"
                        && source_url != request_url;
                    let request_type = map_webview2_resource_context(context.0, is_subframe_doc);

                    if adblocker.check_network_request(&request_url, &source_url, request_type) {
                        if let Ok(blocked_response) =
                            env.CreateWebResourceResponse(None, 204, &status_phrase, &cors_headers)
                        {
                            let _ = args.SetResponse(&blocked_response);
                            on_blocked();
                        }
                    } else {
                        // Privacy signals / cookie policy on requests that are allowed through.
                        let send_signals = adblocker.send_dnt_gpc();
                        let strip_cookies = adblocker.block_third_party_cookies()
                            && context.0 != 1
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
}
