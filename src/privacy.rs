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

/// Common multi-tenant hosting suffixes from the private section of the public suffix list:
/// `alice.github.io` and `bob.github.io` are different sites.
const PRIVATE_SUFFIXES: &[&str] = &[
    "github.io",
    "githubusercontent.com",
    "gitlab.io",
    "blogspot.com",
    "wordpress.com",
    "herokuapp.com",
    "appspot.com",
    "netlify.app",
    "vercel.app",
    "pages.dev",
    "workers.dev",
    "web.app",
    "firebaseapp.com",
    "azurewebsites.net",
    "cloudfront.net",
    "s3.amazonaws.com",
    "fly.dev",
    "onrender.com",
];

/// Approximates the registrable domain (eTLD+1) of `host`.
///
/// IP literals (v4 and bracketed v6) are returned unchanged. Uses a small built-in suffix
/// heuristic instead of the full public suffix list: `<label>.<compound>.<ccTLD>`
/// (`bbc.co.uk`) and a handful of private multi-tenant suffixes (`user.github.io`).
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.starts_with('[')
        || host.contains(':')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n <= 2 {
        return labels.join(".");
    }

    let suffix_labels = PRIVATE_SUFFIXES
        .iter()
        .filter(|suffix| {
            host.strip_suffix(*suffix)
                .is_some_and(|rest| rest.ends_with('.'))
        })
        .map(|suffix| suffix.split('.').count())
        .max();
    let take = if let Some(count) = suffix_labels {
        count + 1
    } else {
        let tld = labels[n - 1];
        let second = labels[n - 2];
        if tld.len() == 2 && COMPOUND_SUFFIX_LABELS.contains(&second) {
            3
        } else {
            2
        }
    };
    labels[n.saturating_sub(take)..].join(".")
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

/// Kinds of browsing data that can be cleared through the WebView2 profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearKinds {
    /// Cookies only (the "Cookies and site data" checkbox).
    pub cookies: bool,
    /// HTTP disk cache and Cache Storage.
    pub cache: bool,
    /// Passwords saved by WebView2's built-in password manager.
    pub passwords: bool,
    /// General autofill data (addresses, form entries).
    pub autofill: bool,
    /// All site data: cookies, DOM storage, IndexedDB, service workers, cache storage, ...
    pub all_site_data: bool,
}

impl ClearKinds {
    /// Parses the comma-separated `clear_data` action argument from `ember://settings`
    /// (`history,cookies,cache,passwords,autofill`). Unknown tokens are ignored.
    pub fn from_action_arg(arg: &str) -> Self {
        let mut kinds = Self::default();
        for token in arg.split(',').map(str::trim) {
            match token {
                "cookies" => kinds.cookies = true,
                "cache" => kinds.cache = true,
                "passwords" => kinds.passwords = true,
                "autofill" => kinds.autofill = true,
                _ => {}
            }
        }
        kinds
    }

    /// Cookies and all other site data, used by "Clear cookies and site data when Ember closes".
    /// Saved passwords and autofill data are kept.
    pub const fn site_data() -> Self {
        Self {
            cookies: true,
            cache: false,
            passwords: false,
            autofill: false,
            all_site_data: true,
        }
    }

    pub const fn is_empty(&self) -> bool {
        !(self.cookies || self.cache || self.passwords || self.autofill || self.all_site_data)
    }
}

/// Clears the selected kinds of browsing data of the WebView2 profile that `webview` uses.
/// Returns `true` if the request was submitted.
pub fn clear_browsing_data(webview: &WebView, kinds: ClearKinds) -> bool {
    platform::clear_browsing_data(webview, kinds, Box::new(|_| {}))
}

/// Like [`clear_browsing_data`], calling `on_done(success)` on the UI thread once WebView2 has
/// finished. `on_done` is never called when this returns `false`.
pub fn clear_browsing_data_then(
    webview: &WebView,
    kinds: ClearKinds,
    on_done: Box<dyn FnOnce(bool)>,
) -> bool {
    platform::clear_browsing_data(webview, kinds, on_done)
}

/// Turns WebView2's built-in password manager ("offer to save passwords"; the runtime stores
/// them encrypted in the profile's user data folder) and general form autofill on or off for
/// one webview. Returns `true` if both settings were applied.
pub fn apply_autofill_settings(webview: &WebView, save_passwords: bool, autofill: bool) -> bool {
    platform::apply_autofill_settings(webview, save_passwords, autofill)
}

#[cfg(target_os = "windows")]
mod platform {
    use super::ClearKinds;
    use webview2_com::{
        ClearBrowsingDataCompletedHandler,
        Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2Profile2, ICoreWebView2Settings4, ICoreWebView2_13,
            COREWEBVIEW2_BROWSING_DATA_KINDS, COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES, COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
            COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL,
            COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE,
        },
    };
    use windows_core::Interface;
    use wry::{WebView, WebViewExtWindows};

    fn core_webview(webview: &WebView) -> Option<ICoreWebView2> {
        let controller = webview.controller();
        // SAFETY: `controller` is a live COM pointer owned by `webview` on the main UI thread.
        unsafe { controller.CoreWebView2() }.ok()
    }

    pub fn kinds_to_bits(kinds: ClearKinds) -> i32 {
        let mut bits = 0;
        if kinds.cookies {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0;
        }
        if kinds.cache {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0
                | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE.0;
        }
        if kinds.passwords {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE.0;
        }
        if kinds.autofill {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL.0;
        }
        if kinds.all_site_data {
            bits |= COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_SITE.0;
        }
        bits
    }

    pub fn clear_browsing_data(
        webview: &WebView,
        kinds: ClearKinds,
        on_done: Box<dyn FnOnce(bool)>,
    ) -> bool {
        let bits = kinds_to_bits(kinds);
        if bits == 0 {
            return false;
        }
        let Some(core) = core_webview(webview) else {
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

        let handler = ClearBrowsingDataCompletedHandler::create(Box::new(move |result| {
            on_done(result.is_ok());
            Ok(())
        }));
        // SAFETY: Asynchronous request; the handler keeps itself alive until completion.
        unsafe {
            profile2
                .ClearBrowsingData(COREWEBVIEW2_BROWSING_DATA_KINDS(bits), &handler)
                .is_ok()
        }
    }

    pub fn apply_autofill_settings(
        webview: &WebView,
        save_passwords: bool,
        autofill: bool,
    ) -> bool {
        let Some(core) = core_webview(webview) else {
            return false;
        };
        // SAFETY: Plain COM property getter on a valid interface.
        let Ok(settings) = (unsafe { core.Settings() }) else {
            return false;
        };
        let Ok(settings4) = settings.cast::<ICoreWebView2Settings4>() else {
            return false;
        };
        // SAFETY: Plain COM property setters on a valid interface, on the UI thread.
        unsafe {
            let passwords_ok = settings4
                .SetIsPasswordAutosaveEnabled(save_passwords)
                .is_ok();
            let autofill_ok = settings4.SetIsGeneralAutofillEnabled(autofill).is_ok();
            passwords_ok && autofill_ok
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::ClearKinds;
    use wry::WebView;

    pub fn clear_browsing_data(
        webview: &WebView,
        kinds: ClearKinds,
        on_done: Box<dyn FnOnce(bool)>,
    ) -> bool {
        if kinds.is_empty() {
            return false;
        }
        let ok = webview.clear_all_browsing_data().is_ok();
        if ok {
            on_done(true);
        }
        ok
    }

    pub fn apply_autofill_settings(_webview: &WebView, _passwords: bool, _autofill: bool) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clear_kinds_from_action_arg() {
        let all = ClearKinds::from_action_arg("history,cookies,cache,passwords,autofill");
        assert!(all.cookies && all.cache && all.passwords && all.autofill);
        assert!(!all.all_site_data);
        assert!(ClearKinds::from_action_arg("history").is_empty());
        // Tokens must match exactly ("passwordsx" is not "passwords").
        assert!(ClearKinds::from_action_arg("passwordsx, autofil").is_empty());
        let pw = ClearKinds::from_action_arg(" passwords ");
        assert!(pw.passwords && !pw.cookies);
        let site = ClearKinds::site_data();
        assert!(site.cookies && site.all_site_data && !site.passwords && !site.autofill);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_clear_kinds_bits() {
        assert_eq!(platform::kinds_to_bits(ClearKinds::default()), 0);
        let pw = ClearKinds {
            passwords: true,
            autofill: true,
            ..ClearKinds::default()
        };
        assert_eq!(platform::kinds_to_bits(pw), 2048 | 1024);
        // ALL_SITE (128) | COOKIES (64); passwords and autofill data are kept.
        assert_eq!(platform::kinds_to_bits(ClearKinds::site_data()), 128 | 64);
    }

    #[test]
    fn test_registrable_domain() {
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("cdn.static.example.com"), "example.com");
        assert_eq!(registrable_domain("news.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(registrable_domain("127.0.0.1"), "127.0.0.1");
    }

    #[test]
    fn test_registrable_domain_edge_cases() {
        assert_eq!(registrable_domain("WWW.Example.COM."), "example.com");
        assert_eq!(registrable_domain("localhost"), "localhost");
        assert_eq!(registrable_domain("[::1]"), "[::1]");
        assert_eq!(registrable_domain("a.b.example.com.au"), "example.com.au");
        assert_eq!(registrable_domain("shop.example.co.jp"), "example.co.jp");
        // 2-letter TLD without a compound second-level label.
        assert_eq!(registrable_domain("cdn.example.io"), "example.io");
        // Private multi-tenant suffixes.
        assert_eq!(registrable_domain("alice.github.io"), "alice.github.io");
        assert_eq!(
            registrable_domain("assets.alice.github.io"),
            "alice.github.io"
        );
        assert_eq!(
            registrable_domain("bucket.s3.amazonaws.com"),
            "bucket.s3.amazonaws.com"
        );
        // A suffix match must be on a label boundary.
        assert_eq!(registrable_domain("www.notgithub.io"), "notgithub.io");
        assert_eq!(registrable_domain("github.io"), "github.io");
        assert_eq!(registrable_domain(""), "");
    }

    #[test]
    fn test_is_third_party_more_cases() {
        // Same site across subdomains / ports / schemes.
        assert!(!is_third_party(
            "http://api.example.com:8080/x",
            "https://example.com/"
        ));
        assert!(!is_third_party(
            "https://img.bbc.co.uk/a.png",
            "https://news.bbc.co.uk/"
        ));
        // Different sites under a compound suffix.
        assert!(is_third_party(
            "https://tracker.co.uk/p",
            "https://news.bbc.co.uk/"
        ));
        // Multi-tenant hosting: different tenants are third parties of each other.
        assert!(is_third_party(
            "https://bob.github.io/x.js",
            "https://alice.github.io/"
        ));
        assert!(!is_third_party(
            "https://alice.github.io/x.js",
            "https://alice.github.io/page"
        ));
        // IP literals.
        assert!(is_third_party("http://10.0.0.2/a", "http://10.0.0.1/"));
        assert!(!is_third_party("http://[::1]:3000/a", "http://[::1]:8080/"));
        // Unknown / non-HTTP sources are first-party.
        assert!(!is_third_party("https://a.com/", ""));
        assert!(!is_third_party("https://a.com/", "data:text/html,hi"));
        assert!(!is_third_party("data:text/plain,x", "https://a.com/"));
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
