//! Pure tab strip layout calculations and element visibility thresholds.
//!
//! Designed to run deterministically with zero DOM or OS dependencies so all
//! geometry, progressive content fading, and overflow decisions can be verified
//! via unit tests.

/// Maximum tab width in logical pixels.
pub const TAB_MAX_WIDTH: u32 = 240;

/// Minimum tab width in logical pixels (enough for favicon and close button).
pub const TAB_MIN_WIDTH: u32 = 36;

/// Horizontal gap between adjacent tabs in logical pixels.
pub const TAB_GAP: u32 = 2;

/// Tab width at or above which the title is fully opaque (1.0).
pub const TITLE_FADE_MAX: u32 = 130;

/// Tab width at or below which the title is completely transparent (0.0) and collapsed.
pub const TITLE_FADE_MIN: u32 = 75;

/// Tab width threshold below which inactive tabs hide their close button (micro tab).
pub const MICRO_TAB_THRESHOLD: u32 = 52;

/// Central animation duration in milliseconds.
pub const TAB_ANIM_DURATION_MS: u32 = 180;

/// Central easing curve representation.
pub const TAB_ANIM_EASE: &str = "cubic-bezier(0.2, 0, 0, 1)";

/// Computes the layout width in pixels for every tab given the count and available strip width.
///
/// Clamped between `TAB_MIN_WIDTH` (36px) and `TAB_MAX_WIDTH` (240px).
pub fn compute_tab_width(tab_count: usize, available_width: u32) -> u32 {
    if tab_count == 0 {
        return TAB_MAX_WIDTH;
    }
    let gaps = (tab_count.saturating_sub(1) as u32).saturating_mul(TAB_GAP);
    let available = available_width.saturating_sub(gaps);
    let raw = available / (tab_count as u32);
    raw.clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
}

/// Computes the title opacity (0.0 to 1.0) based on tab width.
///
/// Returns 1.0 when width >= 130px, 0.0 when width <= 75px, and a linear interpolation in between.
pub fn compute_title_opacity(tab_width: u32) -> f32 {
    if tab_width <= TITLE_FADE_MIN {
        0.0
    } else if tab_width >= TITLE_FADE_MAX {
        1.0
    } else {
        let range = (TITLE_FADE_MAX - TITLE_FADE_MIN) as f32;
        let delta = (tab_width - TITLE_FADE_MIN) as f32;
        let opacity = delta / range;
        (opacity * 100.0).round() / 100.0
    }
}

/// Returns `true` if the tab title text should be hidden from layout (`max-width: 0`).
pub fn is_title_hidden(tab_width: u32) -> bool {
    tab_width <= TITLE_FADE_MIN
}

/// Returns `true` if the tab is in micro state (width <= 52px).
pub fn is_micro_tab(tab_width: u32) -> bool {
    tab_width <= MICRO_TAB_THRESHOLD
}

/// Returns `true` if the inactive tab close button should be visible.
pub fn is_inactive_close_visible(tab_width: u32) -> bool {
    tab_width > MICRO_TAB_THRESHOLD
}

/// Active tab ALWAYS keeps its close button visible.
pub fn is_active_close_visible(_tab_width: u32) -> bool {
    true
}

/// Returns `true` if the tabs exceed the available base width even when shrunk to `TAB_MIN_WIDTH`.
pub fn would_overflow(tab_count: usize, available_base_width: u32) -> bool {
    if tab_count == 0 {
        return false;
    }
    let total_min_width = (tab_count as u32)
        .saturating_mul(TAB_MIN_WIDTH)
        .saturating_add((tab_count.saturating_sub(1) as u32).saturating_mul(TAB_GAP));
    total_min_width > available_base_width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_tab_width_bounds() {
        // 0 tabs fallback to MAX
        assert_eq!(compute_tab_width(0, 1000), TAB_MAX_WIDTH);

        // 1 tab clamped to MAX_WIDTH
        assert_eq!(compute_tab_width(1, 1000), TAB_MAX_WIDTH);
        assert_eq!(compute_tab_width(1, 200), 200);
        assert_eq!(compute_tab_width(1, 30), TAB_MIN_WIDTH);

        // 5 tabs with 918px available: (918 - 8) / 5 = 182
        assert_eq!(compute_tab_width(5, 918), 182);

        // 15 tabs with 918px available: (918 - 28) / 15 = 59
        assert_eq!(compute_tab_width(15, 918), 59);

        // 30 tabs with 918px available: (918 - 58) / 30 = 28 -> clamped to MIN_WIDTH (36)
        assert_eq!(compute_tab_width(30, 918), TAB_MIN_WIDTH);

        // 60 tabs with 918px available: clamped to MIN_WIDTH (36)
        assert_eq!(compute_tab_width(60, 918), TAB_MIN_WIDTH);
    }

    #[test]
    fn test_progressive_title_opacity() {
        assert_eq!(compute_title_opacity(240), 1.0);
        assert_eq!(compute_title_opacity(130), 1.0);
        assert_eq!(compute_title_opacity(75), 0.0);
        assert_eq!(compute_title_opacity(50), 0.0);
        assert_eq!(compute_title_opacity(36), 0.0);

        // Midway between 75 and 130: 102.5 -> (102 - 75) / 55 = 27 / 55 = 0.49
        let mid = compute_title_opacity(102);
        assert!(mid > 0.45 && mid < 0.55);
    }

    #[test]
    fn test_progressive_visibility_thresholds() {
        // >= 130px: title visible, inactive close visible
        assert!(!is_title_hidden(180));
        assert!(!is_micro_tab(180));
        assert!(is_inactive_close_visible(180));
        assert!(is_active_close_visible(180));

        // 60px: title hidden, inactive close STILL visible (60 > 52)
        assert!(is_title_hidden(60));
        assert!(!is_micro_tab(60));
        assert!(is_inactive_close_visible(60));
        assert!(is_active_close_visible(60));

        // 36px (micro): title hidden, inactive close HIDDEN, active close VISIBLE
        assert!(is_title_hidden(36));
        assert!(is_micro_tab(36));
        assert!(!is_inactive_close_visible(36));
        assert!(is_active_close_visible(36));
    }

    #[test]
    fn test_overflow_detection() {
        let available = 918;
        // 5 tabs: 5 * 36 + 4 * 2 = 188 <= 918
        assert!(!would_overflow(5, available));
        // 15 tabs: 15 * 36 + 14 * 2 = 568 <= 918
        assert!(!would_overflow(15, available));
        // 24 tabs: 24 * 36 + 23 * 2 = 864 + 46 = 910 <= 918
        assert!(!would_overflow(24, available));
        // 25 tabs: 25 * 36 + 24 * 2 = 900 + 48 = 948 > 918 -> OVERFLOW
        assert!(would_overflow(25, available));
        // 30 tabs: 30 * 36 + 29 * 2 = 1138 > 918 -> OVERFLOW
        assert!(would_overflow(30, available));
        // 60 tabs: 60 * 36 + 59 * 2 = 2278 > 918 -> OVERFLOW
        assert!(would_overflow(60, available));
    }
}
