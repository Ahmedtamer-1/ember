//! Pure tab strip layout calculations and element visibility thresholds.
//!
//! Designed to run deterministically with zero DOM or OS dependencies so all
//! geometry, progressive content fading, and overflow decisions can be verified
//! via unit tests.

/// Maximum tab width in logical pixels.
pub const TAB_MAX_WIDTH: u32 = 240;

/// Minimum tab width in logical pixels (large enough to fit 14px favicon + 24px close button + padding).
pub const TAB_MIN_WIDTH: u32 = 48;

/// Horizontal gap between adjacent tabs in logical pixels.
pub const TAB_GAP: u32 = 2;

/// Tab width at or above which the title is fully opaque (1.0).
pub const TITLE_FADE_MAX: u32 = 110;

/// Tab width below which the title is hidden (opacity 0.0 and collapsed).
pub const TITLE_FADE_MIN: u32 = 60;

/// Tab width threshold below which inactive tabs hide their close button (micro tab).
pub const MICRO_TAB_THRESHOLD: u32 = 56;

/// Central animation duration in milliseconds.
pub const TAB_ANIM_DURATION_MS: u32 = 180;

/// Central easing curve representation.
pub const TAB_ANIM_EASE: &str = "cubic-bezier(0.2, 0, 0, 1)";

/// Pure width-to-visibility output for a single tab.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabVisibility {
    pub show_title: bool,
    pub title_opacity: f32,
    pub show_close: bool,
    pub is_micro: bool,
}

/// Computes element visibility strictly from a tab's actual rendered width and active state.
pub fn compute_tab_visibility(tab_width: u32, is_active: bool) -> TabVisibility {
    let show_title = tab_width >= TITLE_FADE_MIN;
    let title_opacity = compute_title_opacity(tab_width);
    let is_micro = tab_width < MICRO_TAB_THRESHOLD;
    let show_close = is_active || !is_micro;
    TabVisibility {
        show_title,
        title_opacity,
        show_close,
        is_micro,
    }
}

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
/// Returns 1.0 when width >= 110px, 0.0 when width < 60px, and a linear interpolation in between.
pub fn compute_title_opacity(tab_width: u32) -> f32 {
    if tab_width < TITLE_FADE_MIN {
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
    tab_width < TITLE_FADE_MIN
}

/// Returns `true` if the tab is in micro state (width < 56px).
pub fn is_micro_tab(tab_width: u32) -> bool {
    tab_width < MICRO_TAB_THRESHOLD
}

/// Returns `true` if the inactive tab close button should be visible.
pub fn is_inactive_close_visible(tab_width: u32) -> bool {
    tab_width >= MICRO_TAB_THRESHOLD
}

/// Active tab ALWAYS keeps its close button visible.
pub fn is_active_close_visible(_tab_width: u32) -> bool {
    true
}

/// Computed integer-pixel slot for a single tab in the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabSlot {
    pub id: usize,
    pub left: i32,
    pub width: i32,
}

/// Computes the deterministic integer-pixel left offset and width for every tab in `tab_ids`.
/// When `frozen_width` is `Some(w)`, tabs keep `min(w, natural_width)` until unfrozen (`None`).
pub fn compute_tab_strip_layout(
    tab_ids: &[usize],
    strip_width: u32,
    frozen_width: Option<u32>,
) -> Vec<TabSlot> {
    if tab_ids.is_empty() {
        return Vec::new();
    }
    let natural_width = compute_tab_width(tab_ids.len(), strip_width);
    let slot_width = frozen_width.map_or(natural_width, |fw| fw.min(natural_width));
    let step = slot_width as i32 + TAB_GAP as i32;

    tab_ids
        .iter()
        .enumerate()
        .map(|(idx, &id)| TabSlot {
            id,
            left: (idx as i32) * step,
            width: slot_width as i32,
        })
        .collect()
}

/// Pure diff produced when reconciling the tab strip DOM against an authoritative Rust snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileDiff {
    pub accepted: bool,
    pub next_revision: u64,
    pub to_create: Vec<usize>,
    pub to_keep: Vec<usize>,
    pub to_close: Vec<usize>,
    pub to_remove_immediately: Vec<usize>,
}

/// Reconciles the UI's current surviving and closing tab IDs against Rust's ordered snapshot.
///
/// Snapshots with `next_revision < last_revision` are rejected as stale.
pub fn reconcile_tab_ids(
    last_revision: u64,
    next_revision: u64,
    dom_surviving_ids: &[usize],
    dom_closing_ids: &[usize],
    snapshot_ids: &[usize],
) -> ReconcileDiff {
    if next_revision < last_revision {
        return ReconcileDiff {
            accepted: false,
            next_revision: last_revision,
            to_create: Vec::new(),
            to_keep: Vec::new(),
            to_close: Vec::new(),
            to_remove_immediately: Vec::new(),
        };
    }

    let mut to_create = Vec::new();
    let mut to_keep = Vec::new();
    for &id in snapshot_ids {
        if dom_surviving_ids.contains(&id) {
            to_keep.push(id);
        } else {
            to_create.push(id);
        }
    }

    let mut to_close = Vec::new();
    for &id in dom_surviving_ids {
        if !snapshot_ids.contains(&id) && !dom_closing_ids.contains(&id) {
            to_close.push(id);
        }
    }

    let mut to_remove_immediately = Vec::new();
    for &id in dom_closing_ids {
        // If a tab ID reappears (e.g. reopened or recycled) while a closing node for it exists,
        // remove the old closing node immediately before creating the new one.
        if snapshot_ids.contains(&id) {
            to_remove_immediately.push(id);
        }
    }

    ReconcileDiff {
        accepted: true,
        next_revision,
        to_create,
        to_keep,
        to_close,
        to_remove_immediately,
    }
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
        assert_eq!(compute_title_opacity(110), 1.0);
        assert_eq!(compute_title_opacity(60), 0.0);
        assert_eq!(compute_title_opacity(59), 0.0);
        assert_eq!(compute_title_opacity(48), 0.0);

        // Midway between 60 and 110: 85 -> (85 - 60) / 50 = 0.5
        let mid = compute_title_opacity(85);
        assert!((mid - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_progressive_visibility_thresholds() {
        // Test across widths from 30px to 240px
        for w in 30..=240 {
            let active = compute_tab_visibility(w, true);
            let inactive = compute_tab_visibility(w, false);

            // Active tab ALWAYS shows close button
            assert!(active.show_close, "active close hidden at width {w}");
            assert_eq!(active.show_title, w >= TITLE_FADE_MIN);
            assert_eq!(inactive.show_title, w >= TITLE_FADE_MIN);
            assert_eq!(inactive.show_close, w >= MICRO_TAB_THRESHOLD);
            assert_eq!(inactive.is_micro, w < MICRO_TAB_THRESHOLD);
        }

        // Spot-check boundary widths
        assert!(is_title_hidden(59));
        assert!(!is_title_hidden(60));
        assert!(is_micro_tab(55));
        assert!(!is_inactive_close_visible(55));
        assert!(is_inactive_close_visible(56));
        assert!(is_active_close_visible(48));
    }

    #[test]
    fn test_overflow_detection() {
        let available = 918;
        // 5 tabs: 5 * 48 + 4 * 2 = 248 <= 918
        assert!(!would_overflow(5, available));
        // 15 tabs: 15 * 48 + 14 * 2 = 748 <= 918
        assert!(!would_overflow(15, available));
        // 18 tabs: 18 * 48 + 17 * 2 = 864 + 34 = 898 <= 918
        assert!(!would_overflow(18, available));
        // 19 tabs: 19 * 48 + 18 * 2 = 912 + 36 = 948 > 918 -> OVERFLOW
        assert!(would_overflow(19, available));
        // 30 tabs: 30 * 48 + 29 * 2 = 1498 > 918 -> OVERFLOW
        assert!(would_overflow(30, available));
        // 60 tabs: 60 * 48 + 59 * 2 = 2998 > 918 -> OVERFLOW
        assert!(would_overflow(60, available));
    }

    #[test]
    fn test_compute_tab_strip_layout_close_first_middle_last() {
        let strip = 918;
        // 5 tabs -> width 182, step 184
        let initial = compute_tab_strip_layout(&[1, 2, 3, 4, 5], strip, None);
        assert_eq!(initial.len(), 5);
        assert_eq!(
            initial[0],
            TabSlot {
                id: 1,
                left: 0,
                width: 182
            }
        );
        assert_eq!(
            initial[4],
            TabSlot {
                id: 5,
                left: 736,
                width: 182
            }
        );

        // Close first tab (1) -> 4 tabs -> width (918 - 6) / 4 = 228, step 230
        let after_first = compute_tab_strip_layout(&[2, 3, 4, 5], strip, None);
        assert_eq!(
            after_first[0],
            TabSlot {
                id: 2,
                left: 0,
                width: 228
            }
        );
        assert_eq!(
            after_first[3],
            TabSlot {
                id: 5,
                left: 690,
                width: 228
            }
        );

        // Close middle tab (3) -> 3 tabs -> width 240 (clamped), step 242
        let after_middle = compute_tab_strip_layout(&[2, 4, 5], strip, None);
        assert_eq!(
            after_middle[0],
            TabSlot {
                id: 2,
                left: 0,
                width: 240
            }
        );
        assert_eq!(
            after_middle[1],
            TabSlot {
                id: 4,
                left: 242,
                width: 240
            }
        );
        assert_eq!(
            after_middle[2],
            TabSlot {
                id: 5,
                left: 484,
                width: 240
            }
        );

        // Close last tab (5) -> 2 tabs -> width 240
        let after_last = compute_tab_strip_layout(&[2, 4], strip, None);
        assert_eq!(
            after_last,
            vec![
                TabSlot {
                    id: 2,
                    left: 0,
                    width: 240
                },
                TabSlot {
                    id: 4,
                    left: 242,
                    width: 240
                },
            ]
        );
    }

    #[test]
    fn test_compute_tab_strip_layout_overflow_and_frozen_and_rapid_close() {
        let strip = 918;
        // 25 tabs -> overflow at TAB_MIN_WIDTH (48), step 50
        let ids_25: Vec<usize> = (1..=25).collect();
        let overflow_layout = compute_tab_strip_layout(&ids_25, strip, None);
        assert_eq!(overflow_layout[0].width, TAB_MIN_WIDTH as i32);
        assert_eq!(
            overflow_layout[24].left,
            24 * (TAB_MIN_WIDTH as i32 + TAB_GAP as i32)
        );

        // 6 tabs: natural width = (918 - 10) / 6 = 151.
        // Mouse-closing tab 2 freezes remaining 5 tabs at 151px instead of expanding to 182px.
        let frozen = compute_tab_strip_layout(&[1, 3, 4, 5, 6], strip, Some(151));
        assert_eq!(
            frozen[0],
            TabSlot {
                id: 1,
                left: 0,
                width: 151
            }
        );
        assert_eq!(
            frozen[1],
            TabSlot {
                id: 3,
                left: 153,
                width: 151
            }
        );

        // Rapidly closing 3 more tabs while frozen keeps width at 151px and shifts integer lefts.
        let rapid = compute_tab_strip_layout(&[1, 6], strip, Some(151));
        assert_eq!(
            rapid,
            vec![
                TabSlot {
                    id: 1,
                    left: 0,
                    width: 151
                },
                TabSlot {
                    id: 6,
                    left: 153,
                    width: 151
                },
            ]
        );

        // Unfreezing on mouseleave expands cleanly to natural width (240px).
        let unfrozen = compute_tab_strip_layout(&[1, 6], strip, None);
        assert_eq!(
            unfrozen,
            vec![
                TabSlot {
                    id: 1,
                    left: 0,
                    width: 240
                },
                TabSlot {
                    id: 6,
                    left: 242,
                    width: 240
                },
            ]
        );
    }

    #[test]
    fn test_reconcile_tab_ids_diff_and_out_of_order() {
        // Initial open of [1, 2, 3] at revision 1
        let d1 = reconcile_tab_ids(0, 1, &[], &[], &[1, 2, 3]);
        assert!(d1.accepted);
        assert_eq!(d1.to_create, vec![1, 2, 3]);
        assert!(d1.to_keep.is_empty());
        assert!(d1.to_close.is_empty());

        // Stale revision 0 is rejected
        let stale = reconcile_tab_ids(2, 1, &[1, 2, 3], &[], &[1]);
        assert!(!stale.accepted);
        assert_eq!(stale.next_revision, 2);

        // Close tab 2 and open tab 4 at revision 2
        let d2 = reconcile_tab_ids(1, 2, &[1, 2, 3], &[], &[1, 3, 4]);
        assert!(d2.accepted);
        assert_eq!(d2.to_keep, vec![1, 3]);
        assert_eq!(d2.to_create, vec![4]);
        assert_eq!(d2.to_close, vec![2]);
        assert!(d2.to_remove_immediately.is_empty());
    }

    #[test]
    fn test_reconcile_tab_ids_retarget_during_close_and_rapid_close() {
        // Tab 2 is currently closing; another snapshot arrives closing tab 3 as well.
        // Tab 2 must NOT be re-closed or duplicated, and tab 3 is added to to_close.
        let d1 = reconcile_tab_ids(5, 6, &[1, 3, 4], &[2], &[1, 4]);
        assert!(d1.accepted);
        assert_eq!(d1.to_keep, vec![1, 4]);
        assert_eq!(d1.to_close, vec![3]);
        assert!(d1.to_create.is_empty());

        // Rapid close of 20 tabs down to 1 active tab
        let initial: Vec<usize> = (1..=21).collect();
        let rapid = reconcile_tab_ids(6, 7, &initial, &[], &[21]);
        assert!(rapid.accepted);
        assert_eq!(rapid.to_keep, vec![21]);
        assert_eq!(rapid.to_close, (1..=20).collect::<Vec<_>>());
        assert!(rapid.to_create.is_empty());

        // If a closing ID reappears in snapshot, it is removed immediately from closing set.
        let reopen = reconcile_tab_ids(7, 8, &[1], &[2], &[1, 2]);
        assert_eq!(reopen.to_remove_immediately, vec![2]);
        assert_eq!(reopen.to_create, vec![2]);
    }
}
