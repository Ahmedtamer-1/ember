//! Privacy helpers: third-party request detection, clearing browsing data through the WebView2
//! profile, and the `navigator.doNotTrack` / `navigator.globalPrivacyControl` page script.

use wry::WebView;

/// Page script exposing the Do Not Track / Global Privacy Control signals to JavaScript.
/// The matching `DNT: 1` / `Sec-GPC: 1` request headers are added natively by the adblocker's
/// `WebResourceRequested` handler.
pub const DNT_GPC_SCRIPT: &str = r#"
(function () {
  try {
    Object.defineProperty(Navigator.prototype, "globalPrivacyControl", {
      get: function () { return true; },
      configurable: true
    });
    Object.defineProperty(Navigator.prototype, "doNotTrack", {
      get: function () { return "1"; },
      configurable: true
    });
  } catch (_) {}
})();
"#;

/// Second-level labels that form part of a public suffix when followed by a 2-letter ccTLD
/// (`example.co.uk`, `example.com.au`, ...). A tiny stand-in for the full public suffix list.
const COMPOUND_SUFFIX_LABELS: &[&str] =
    &["co", "com", "org", "net", "gov", "edu", "ac", "or", "ne"];

/// Approximates the registrable domain (eTLD+1) of `host`.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.starts_with('[')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return host;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host;
    }
    let tld = labels[n - 1];
    let second = labels[n - 2];
    let take = if tld.len() == 2 && COMPOUND_SUFFIX_LABELS.contains(&second) {
        3
    } else {
        2
    };
    labels[n - take..].join(".")
}

/// Returns `true` if `request_url` belongs to a different registrable domain than `source_url`.
/// Unknown / non-HTTP(S) sources are treated as first-party.
pub fn is_third_party(request_url: &str, source_url: &str) -> bool {
    let Some(req_host) = crate::adblock::extract_normalized_host(request_url) else {
        return false;
    };
    let Some(src_host) = crate::adblock::extract_normalized_host(source_url) else {
        return false;
    };
    registrable_domain(&req_host) != registrable_domain(&src_host)
}

/// Clears cookies and/or the HTTP cache of the shared WebView2 profile.
/// Returns `true` if the request was submitted.
pub fn clear_browsing_data(webview: &WebView, cookies: bool, cache: bool) -> bool {
    platform::clear_browsing_data(webview, cookies, cache)
}

#[cfg(target_os = "windows")]
mod platform {
    use webview2_com::{
        ClearBrowsingDataCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2Profile2, ICoreWebView2_13, COREWEBVIEW2_BROWSING_DATA_KINDS,
            COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES, COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
        },
    };
    use windows_core::Interface;
    use wry::{WebView, WebViewExtWindows};

    pub fn clear_browsing_data(webview: &WebView, cookies: bool, cache: bool) -> bool {
        let mut bits = 0;
        if cookies {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0;
        }
        if cache {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0
                | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE.0;
        }
        if bits == 0 {
            return false;
        }

        let controller = webview.controller();
        // SAFETY: `controller` is a live COM pointer owned by `webview` on the main UI thread.
        let Ok(core) = (unsafe { controller.CoreWebView2() }) else {
            return false;
        };
        let Ok(core13) = core.cast::<ICoreWebView2_13>() else {
            return false;
        };
        // SAFETY: Plain COM property getter on a valid interface.
        let Ok(profile) = (unsafe { core13.Profile() }) else {
            return false;
        };
        let Ok(profile2) = profile.cast::<ICoreWebView2Profile2>() else {
            return false;
        };

        let handler = ClearBrowsingDataCompletedHandler::create(Box::new(|_| Ok(())));
        // SAFETY: Asynchronous request; the handler keeps itself alive until completion.
        unsafe {
            profile2
                .ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS(bits), &handler)
                .is_ok()
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use wry::WebView;

    pub fn clear_browsing_data(webview: &WebView, _cookies: bool, _cache: bool) -> bool {
        webview.clear_all_browsing_data().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registrable_domain() {
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("cdn.static.example.com"), "example.com");
        assert_eq!(registrable_domain("news.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable_domain("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn test_is_third_party() {
        assert!(!is_third_party(
            "https://static.example.com/a.js",
            "https://www.example.com/"
        ));
        assert!(is_third_party(
            "https://tracker.adnet.io/p.gif",
            "https://www.example.com/"
        ));
        assert!(!is_third_party("https://example.com/", "about:blank"));
    }
}
