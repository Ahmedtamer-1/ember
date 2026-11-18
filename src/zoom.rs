/// Standard Chrome-like zoom presets (50% to 300%).
pub const ZOOM_PRESETS: &[f64] = &[
    0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

/// Steps zoom up to the next preset level (capped at `3.0`).
pub fn zoom_in(current: f64) -> f64 {
    for &preset in ZOOM_PRESETS {
        if preset > current + 0.01 {
            return preset;
        }
    }
    3.0
}

/// Steps zoom down to the previous preset level (floored at `0.5`).
pub fn zoom_out(current: f64) -> f64 {
    for &preset in ZOOM_PRESETS.iter().rev() {
        if preset < current - 0.01 {
            return preset;
        }
    }
    0.5
}

/// Returns the default 100% zoom factor (`1.0`).
pub const fn zoom_reset() -> f64 {
    1.0
}

/// Converts a zoom scale factor (`1.25`) to an integer percentage (`125`).
pub fn zoom_percent(factor: f64) -> u32 {
    (factor.clamp(0.25, 5.0) * 100.0).round() as u32
}

/// Extracts the normalized hostname from an HTTP/HTTPS URL for per-site zoom persistence.
///
/// Userinfo (`user:pass@`) and the port are stripped; bracketed IPv6 literals are kept with
/// their brackets (`http://[::1]:8080/` yields `"[::1]"`).
pub fn extract_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let after_scheme = trimmed.find("://").map(|i| &trimmed[i + 3..])?;
    let authority = after_scheme.split(['/', '\\', '?', '#']).next()?;
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let host = if host_port.starts_with('[') {
        let end = host_port.find(']')?;
        &host_port[..=end]
    } else {
        host_port.split(':').next()?
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() || host == "[]" {
        None
    } else {
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zoom_stepping_and_bounds() {
        let z0 = zoom_reset();
        assert_eq!(zoom_percent(z0), 100);

        let z1 = zoom_in(z0);
        assert_eq!(zoom_percent(z1), 110);

        let z2 = zoom_in(z1);
        assert_eq!(zoom_percent(z2), 125);

        let back = zoom_out(z2);
        assert_eq!(zoom_percent(back), 110);

        assert_eq!(zoom_percent(zoom_out(0.5)), 50);
        assert_eq!(zoom_percent(zoom_in(3.0)), 300);
    }

    #[test]
    fn test_extract_host_basic_and_port() {
        assert_eq!(
            extract_host("https://Example.COM/path?q=1"),
            Some(String::from("example.com"))
        );
        assert_eq!(
            extract_host("http://localhost:8080/"),
            Some(String::from("localhost"))
        );
        assert_eq!(
            extract_host("https://example.com#frag"),
            Some(String::from("example.com"))
        );
        assert_eq!(extract_host("ember://settings"), None);
        assert_eq!(extract_host("https:///nohost"), None);
    }

    #[test]
    fn test_extract_host_ipv6_literals() {
        assert_eq!(
            extract_host("http://[::1]:8080/"),
            Some(String::from("[::1]"))
        );
        assert_eq!(extract_host("http://[::1]/"), Some(String::from("[::1]")));
        assert_eq!(
            extract_host("https://[2001:DB8::1]/x"),
            Some(String::from("[2001:db8::1]"))
        );
        assert_eq!(extract_host("http://[::1/"), None);
    }

    #[test]
    fn test_extract_host_strips_userinfo() {
        assert_eq!(
            extract_host("https://user:pass@example.com:8443/a"),
            Some(String::from("example.com"))
        );
        assert_eq!(
            extract_host("https://a@b:c@example.org/"),
            Some(String::from("example.org"))
        );
        assert_eq!(
            extract_host("http://user@[::1]:3000/"),
            Some(String::from("[::1]"))
        );
        // '@' in the path is not userinfo.
        assert_eq!(
            extract_host("https://example.com/@someone"),
            Some(String::from("example.com"))
        );
    }
}
