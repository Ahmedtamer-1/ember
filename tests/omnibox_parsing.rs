use ember::settings::SearchEngine;
use ember::ui::normalize_omnibox_input;

#[test]
fn test_integration_omnibox_direct_urls() {
    assert_eq!(
        normalize_omnibox_input(
            "https://github.com/rust-lang/rust",
            SearchEngine::DuckDuckGo
        ),
        "https://github.com/rust-lang/rust"
    );
    assert_eq!(
        normalize_omnibox_input("http://insecure.example.com/test", SearchEngine::Google),
        "http://insecure.example.com/test"
    );
    assert_eq!(
        normalize_omnibox_input("about:blank", SearchEngine::Brave),
        "about:blank"
    );
}

#[test]
fn test_integration_omnibox_bare_domains_and_ports() {
    assert_eq!(
        normalize_omnibox_input("crates.io", SearchEngine::DuckDuckGo),
        "https://crates.io"
    );
    assert_eq!(
        normalize_omnibox_input("crates.io/crates/wry", SearchEngine::Google),
        "https://crates.io/crates/wry"
    );
    assert_eq!(
        normalize_omnibox_input("localhost:3000", SearchEngine::DuckDuckGo),
        "http://localhost:3000"
    );
    assert_eq!(
        normalize_omnibox_input("127.0.0.1:8080/dashboard", SearchEngine::Bing),
        "http://127.0.0.1:8080/dashboard"
    );
}

#[test]
fn test_integration_omnibox_internal_ember_pages() {
    assert_eq!(
        normalize_omnibox_input("ember://settings", SearchEngine::DuckDuckGo),
        "ember://settings"
    );
    assert_eq!(
        normalize_omnibox_input("ember://history", SearchEngine::Google),
        "ember://history"
    );
    assert_eq!(
        normalize_omnibox_input("ember://bookmarks", SearchEngine::Brave),
        "ember://bookmarks"
    );
    assert_eq!(
        normalize_omnibox_input("ember://newtab", SearchEngine::Startpage),
        "ember://newtab"
    );
}

#[test]
fn test_integration_omnibox_search_query_generation() {
    // DuckDuckGo
    assert_eq!(
        normalize_omnibox_input("rust programming language", SearchEngine::DuckDuckGo),
        "https://duckduckgo.com/?q=rust+programming+language"
    );

    // Google
    assert_eq!(
        normalize_omnibox_input("fast desktop webview", SearchEngine::Google),
        "https://www.google.com/search?q=fast+desktop+webview"
    );

    // Brave
    assert_eq!(
        normalize_omnibox_input("privacy adblocker", SearchEngine::Brave),
        "https://search.brave.com/search?q=privacy+adblocker"
    );

    // Bing
    assert_eq!(
        normalize_omnibox_input("windows 11 snap layouts", SearchEngine::Bing),
        "https://www.bing.com/search?q=windows+11+snap+layouts"
    );

    // Startpage
    assert_eq!(
        normalize_omnibox_input("lightweight browser rust", SearchEngine::Startpage),
        "https://www.startpage.com/sp/search?query=lightweight+browser+rust"
    );

    // Special symbols URL encoding
    assert_eq!(
        normalize_omnibox_input("c++ & rust: 100% fast", SearchEngine::DuckDuckGo),
        "https://duckduckgo.com/?q=c%2B%2B+%26+rust%3A+100%25+fast"
    );
}

#[test]
fn test_integration_internal_navigation_rejects_unsafe_schemes() {
    use ember::ui::resolve_internal_navigation;
    assert_eq!(
        resolve_internal_navigation("crates.io", SearchEngine::DuckDuckGo),
        Some(String::from("https://crates.io"))
    );
    assert_eq!(
        resolve_internal_navigation("file:///C:/Windows/win.ini", SearchEngine::DuckDuckGo),
        None
    );
}

#[test]
fn test_integration_untrusted_page_cannot_send_privileged_ipc() {
    use ember::ui::{internal_ipc_token, parse_page_ipc_event_checked};
    let tok = internal_ipc_token();
    // A web page without the token cannot change settings, even if it guesses the format.
    assert_eq!(
        parse_page_ipc_event_checked(
            "settings_set:adblock_enabled=false",
            tok,
            Some("https://evil.example/"),
            "https://evil.example/"
        ),
        None
    );
    let forged = format!("ember_tok:{tok}:history_clear:");
    // Even the right token is refused when the sending tab is not an internal page.
    assert_eq!(
        parse_page_ipc_event_checked(&forged, tok, Some("https://evil.example/"), ""),
        None
    );
    assert!(parse_page_ipc_event_checked(&forged, tok, Some("ember://history"), "").is_some());
}
