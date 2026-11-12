use ember::adblock::{
    extract_normalized_host, extract_normalized_host_str, map_webview2_resource_context, Adblocker,
};

#[test]
fn test_integration_network_request_blocking() {
    let rules = [
        "||doubleclick.net^$third-party",
        "||ads.example.com^$image",
        "/tracking/beacon.gif$image",
        "||analytics.evil.com^$script",
        "@@||doubleclick.net^$domain=whitelisted-site.com",
    ];

    let blocker = Adblocker::from_rules(&rules);
    assert!(blocker.is_enabled());

    // Block doubleclick third-party script
    assert!(blocker.check_network_request(
        "https://doubleclick.net/ad.js",
        "https://news.com/article",
        "script"
    ));

    // Block ads image
    assert!(blocker.check_network_request(
        "https://ads.example.com/banner.png",
        "https://example.com/",
        "image"
    ));

    // Allow benign content
    assert!(!blocker.check_network_request(
        "https://example.com/styles.css",
        "https://example.com/",
        "stylesheet"
    ));

    // Test exception rule: doubleclick is whitelisted on whitelisted-site.com
    assert!(!blocker.check_network_request(
        "https://doubleclick.net/ad.js",
        "https://whitelisted-site.com/",
        "script"
    ));

    // Master switch disable
    blocker.set_enabled(false);
    assert!(!blocker.check_network_request(
        "https://doubleclick.net/ad.js",
        "https://news.com/article",
        "script"
    ));
}

#[test]
fn test_integration_per_site_allowlist_manager() {
    let rules = ["||ads.bad.com^"];
    let blocker = Adblocker::from_rules(&rules);

    let ad_url = "https://ads.bad.com/banner.png";
    let site_url = "https://my-favorite-blog.com/post/1";

    // Blocked by default
    assert!(blocker.check_network_request(ad_url, site_url, "image"));
    assert!(!blocker.is_site_allowlisted(site_url));

    // Allowlist the site
    assert!(blocker.set_site_allowlisted("my-favorite-blog.com", true));
    assert!(blocker.is_site_allowlisted(site_url));
    assert!(blocker
        .allowlist_hosts()
        .contains(&String::from("my-favorite-blog.com")));

    // Now ads on this site are allowed through
    assert!(!blocker.check_network_request(ad_url, site_url, "image"));

    // Remove from allowlist
    assert!(blocker.set_site_allowlisted("my-favorite-blog.com", false));
    assert!(!blocker.is_site_allowlisted(site_url));

    // Blocked again
    assert!(blocker.check_network_request(ad_url, site_url, "image"));
}

#[test]
fn test_integration_host_normalization_and_zero_copy() {
    assert_eq!(
        extract_normalized_host_str("https://www.example.com/path?arg=1#frag"),
        Some("example.com")
    );
    assert_eq!(
        extract_normalized_host_str("http://sub.domain.co.uk:8080/"),
        Some("sub.domain.co.uk")
    );
    assert_eq!(
        extract_normalized_host_str("user:pass@internal.net"),
        Some("internal.net")
    );
    assert_eq!(
        extract_normalized_host("https://WWW.UPPERCASE.COM/"),
        Some(String::from("uppercase.com"))
    );
}

#[test]
fn test_integration_webview2_resource_context_mapping() {
    // 1 => Document or Subdocument
    assert_eq!(map_webview2_resource_context(1, false), "document");
    assert_eq!(map_webview2_resource_context(1, true), "subdocument");

    // Standard types
    assert_eq!(map_webview2_resource_context(2, false), "stylesheet");
    assert_eq!(map_webview2_resource_context(3, false), "image");
    assert_eq!(map_webview2_resource_context(6, false), "script");
    assert_eq!(map_webview2_resource_context(7, false), "xmlhttprequest");
    assert_eq!(map_webview2_resource_context(8, false), "fetch");
    assert_eq!(map_webview2_resource_context(11, false), "websocket");
    assert_eq!(map_webview2_resource_context(14, false), "ping");
}

#[test]
fn test_integration_cosmetic_filtering_rules() {
    let rules = [
        "example.com##.ad-banner",
        "example.com###sponsored-post",
        "##.generic-popup-ad",
        "example.com#@#.ad-banner",
    ];

    let blocker = Adblocker::from_rules(&rules);
    let resources = blocker.cosmetic_resources_for_url("https://example.com/news");

    // Un-exceptioned specific selector
    assert!(resources
        .hide_selectors
        .contains(&String::from("#sponsored-post")));
    // Exceptioned selector
    assert!(resources.exceptions.contains(".ad-banner"));
}
