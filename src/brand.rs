//! Ember brand assets: the logo mark as inline SVG and a procedural raster for icons.
//!
//! The SVG constants are self-contained (no scripts, no external references, ids prefixed
//! with `ember-logo-`) so they can be inlined into internal `ember://` pages. The raster
//! code in `brand_raster.rs` is shared with `build.rs`, which bakes the same mark into the
//! Windows executable icon.

mod raster {
    include!("brand_raster.rs");
}

pub use raster::render_logo_rgba;

/// Full-color Ember mark (256x256 viewBox, sized by the embedding CSS).
pub const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" role="img" aria-label="Ember"><defs><linearGradient id="ember-logo-trail" gradientUnits="userSpaceOnUse" x1="46" y1="219" x2="222" y2="43"><stop offset="0" stop-color="#C2187F"/><stop offset=".55" stop-color="#FF3D5E"/><stop offset="1" stop-color="#FF9A1F"/></linearGradient><radialGradient id="ember-logo-core" cx="134" cy="131" r="33" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#FFF7DB"/><stop offset=".5" stop-color="#FFD045"/><stop offset="1" stop-color="#FF8F1F"/></radialGradient><radialGradient id="ember-logo-spark" cx="198.3" cy="77" r="29" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#FFF7DB"/><stop offset=".5" stop-color="#FFD045"/><stop offset="1" stop-color="#FF9F1C"/></radialGradient></defs><path d="M211.5 175.8L206.8 184L201.1 191.6L194.7 198.7L187.4 205L179.5 210.5L171.1 215.2L162.1 219L152.8 221.9L143.1 223.7L133.3 224.5L123.4 224.3L113.6 223L103.9 220.7L94.5 217.3L85.5 213L76.9 207.7L68.9 201.5L61.6 194.5L55 186.7L49.3 178.3L44.5 169.2L40.6 159.7L37.8 149.8L36 139.6L35.3 129.2L35.8 118.8L37.3 108.4L39.9 98.3L43.5 88.4L48.2 79L53.9 70L60.6 61.7L68.1 54.1L76.3 47.4L85.3 41.5L94.9 36.5L104.9 32.6L115.4 29.8L126.1 28.1L137 27.5L147.9 28.1L158.8 29.9L169.4 32.7L179.6 36.7L189.5 41.8L198.8 47.9L207.4 54.9L215.2 62.9L181.5 91.1L177.3 86.1L172.5 81.6L167.2 77.6L161.6 74.1L155.5 71.2L149.2 69L142.6 67.4L135.9 66.5L129.1 66.4L122.2 66.9L115.5 68.2L108.8 70.2L102.4 72.9L96.3 76.3L90.6 80.4L85.2 85L80.4 90.2L76.1 95.9L72.5 102L69.4 108.6L67.1 115.4L65.5 122.5L64.7 129.7L64.6 137.1L65.3 144.4L66.8 151.7L69 158.8L72 165.6L75.7 172.1L80.1 178.3L85.2 183.9L90.8 189L97 193.6L103.6 197.4L110.7 200.6L118 203L125.7 204.6L133.5 205.5L141.3 205.5L149.2 204.7L157 203L164.6 200.5L171.9 197.3L178.9 193.2L185.5 188.4L191.6 183L197.1 176.9L202 170.3Z" fill="url(#ember-logo-trail)"/><circle cx="206.7" cy="173" r="5.5" fill="url(#ember-logo-trail)"/><circle cx="134" cy="131" r="33" fill="url(#ember-logo-core)"/><circle cx="198.3" cy="77" r="29" fill="url(#ember-logo-spark)"/></svg>"##;

/// Single-color Ember mark that inherits `currentColor`.
pub const LOGO_MONO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256" role="img" aria-label="Ember" fill="currentColor"><path d="M211.5 175.8L206.8 184L201.1 191.6L194.7 198.7L187.4 205L179.5 210.5L171.1 215.2L162.1 219L152.8 221.9L143.1 223.7L133.3 224.5L123.4 224.3L113.6 223L103.9 220.7L94.5 217.3L85.5 213L76.9 207.7L68.9 201.5L61.6 194.5L55 186.7L49.3 178.3L44.5 169.2L40.6 159.7L37.8 149.8L36 139.6L35.3 129.2L35.8 118.8L37.3 108.4L39.9 98.3L43.5 88.4L48.2 79L53.9 70L60.6 61.7L68.1 54.1L76.3 47.4L85.3 41.5L94.9 36.5L104.9 32.6L115.4 29.8L126.1 28.1L137 27.5L147.9 28.1L158.8 29.9L169.4 32.7L179.6 36.7L189.5 41.8L198.8 47.9L207.4 54.9L215.2 62.9L181.5 91.1L177.3 86.1L172.5 81.6L167.2 77.6L161.6 74.1L155.5 71.2L149.2 69L142.6 67.4L135.9 66.5L129.1 66.4L122.2 66.9L115.5 68.2L108.8 70.2L102.4 72.9L96.3 76.3L90.6 80.4L85.2 85L80.4 90.2L76.1 95.9L72.5 102L69.4 108.6L67.1 115.4L65.5 122.5L64.7 129.7L64.6 137.1L65.3 144.4L66.8 151.7L69 158.8L72 165.6L75.7 172.1L80.1 178.3L85.2 183.9L90.8 189L97 193.6L103.6 197.4L110.7 200.6L118 203L125.7 204.6L133.5 205.5L141.3 205.5L149.2 204.7L157 203L164.6 200.5L171.9 197.3L178.9 193.2L185.5 188.4L191.6 183L197.1 176.9L202 170.3Z" fill="currentColor"/><circle cx="206.7" cy="173" r="5.5" fill="currentColor"/><circle cx="134" cy="131" r="33" fill="currentColor"/><circle cx="198.3" cy="77" r="29" fill="currentColor"/></svg>"##;

/// Pixel size of the runtime window (title bar / Alt+Tab small) icon.
pub const WINDOW_ICON_SIZE: u32 = 32;
/// Pixel size of the runtime taskbar (big) icon on Windows.
pub const TASKBAR_ICON_SIZE: u32 = 64;

/// Returns the Ember mark as a `data:` URI suitable for `<img src>` or a favicon.
pub fn logo_data_uri() -> String {
    let mut out = String::with_capacity(LOGO_SVG.len() + 32);
    out.push_str("data:image/svg+xml,");
    for ch in LOGO_SVG.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '"' => out.push('\''),
            '\n' | '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Builds a `size`x`size` window icon from the procedural raster. Returns `None` if the
/// platform rejects the buffer (the window then keeps its default icon).
pub fn icon(size: u32) -> Option<tao::window::Icon> {
    tao::window::Icon::from_rgba(render_logo_rgba(size), size, size).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(buf: &[u8], size: u32, x: u32, y: u32) -> u8 {
        buf[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn raster_has_expected_buffer_sizes() {
        for size in [1, 16, 24, 32, 48, 64, 256] {
            assert_eq!(render_logo_rgba(size).len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn raster_has_ink_and_transparent_corners() {
        for size in [16, 32, 64] {
            let buf = render_logo_rgba(size);
            let opaque = buf.iter().skip(3).step_by(4).filter(|&&a| a > 200).count();
            assert!(
                opaque > (size * size / 8) as usize,
                "size {size}: too little ink"
            );
            let last = size - 1;
            for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
                assert_eq!(
                    alpha_at(&buf, size, x, y),
                    0,
                    "size {size}: corner ({x},{y})"
                );
            }
            let mid = size / 2;
            assert!(alpha_at(&buf, size, mid, mid) > 200, "core should be solid");
        }
    }

    #[test]
    fn raster_core_is_warm_and_bright() {
        let size = 64;
        let buf = render_logo_rgba(size);
        // Design-space core center (134, 131) maps to roughly (33, 32) at 64px.
        let i = ((32 * size + 33) * 4) as usize;
        let (r, g, b) = (buf[i], buf[i + 1], buf[i + 2]);
        assert!(r > 240 && g > 180 && b < 230, "core color {r},{g},{b}");
    }

    #[test]
    fn window_icons_build() {
        assert!(icon(WINDOW_ICON_SIZE).is_some());
        assert!(icon(TASKBAR_ICON_SIZE).is_some());
    }

    #[test]
    fn svgs_are_self_contained() {
        for svg in [LOGO_SVG, LOGO_MONO_SVG] {
            let lower = svg.to_ascii_lowercase();
            assert!(lower.starts_with("<svg") && lower.ends_with("</svg>"));
            assert!(!lower.contains("<script"));
            assert!(!lower.contains("href"));
            assert!(!lower.contains("<image") && !lower.contains("@import"));
            // The only URL allowed is the SVG namespace declaration.
            assert_eq!(lower.matches("http").count(), 1);
            assert!(lower.contains("xmlns=\"http://www.w3.org/2000/svg\""));
        }
        assert!(LOGO_SVG.contains("id=\"ember-logo-trail\""));
        assert!(LOGO_MONO_SVG.contains("currentColor"));
        assert!(!LOGO_MONO_SVG.contains("id="));
    }

    #[test]
    fn data_uri_is_escaped() {
        let uri = logo_data_uri();
        assert!(uri.starts_with("data:image/svg+xml,%3Csvg"));
        assert!(!uri.contains('#') && !uri.contains('"') && !uri.contains('<'));
    }
}
