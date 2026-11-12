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
pub fn extract_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    let after_scheme = trimmed.find("://").map(|i| &trimmed[i + 3..])?;
    let host_port = after_scheme.split(['/', '?', '#']).next()?;
    let host = host_port.split(':').next()?.trim().to_ascii_lowercase();
    if host.is_empty() {
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
}
