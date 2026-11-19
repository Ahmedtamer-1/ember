//! Custom themes: a user-editable set of colors, shapes, fonts and backgrounds that override
//! the built-in light/dark palettes of the browser chrome and the internal `ember://` pages.
//!
//! A theme is a flat map of whitelisted keys to strictly validated values. Values end up
//! inside `<style>` elements, so every value is checked against its [`Kind`] before it is
//! stored; anything that could break out of a CSS declaration (`;`, `{`, `}`, `<`, quotes in
//! colors, `url(` outside a base64 image data URL, ...) is rejected.
//!
//! The editor lives at `ember://themes`. It sends the whole theme as `key=value` lines over
//! privileged internal IPC (`theme_preview:` / `theme_save:` / `theme_reset`). Themes are
//! persisted in `theme.txt` next to `settings.toml`; JSON import/export is done by the editor.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Largest accepted background image data URL (bytes).
pub const MAX_IMAGE_DATA_URL_LEN: usize = 6 * 1024 * 1024;

/// What a theme key holds and how it is validated.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// A color for a chrome CSS variable (`--name`).
    ChromeColor(&'static str),
    /// A color for an internal-page CSS variable (`--name`).
    PageColor(&'static str),
    /// Any color used by generated rules (gradients, glow).
    Color,
    /// A number clamped to `[min, max]`.
    Number(f64, f64),
    /// One of a fixed set of words.
    Choice(&'static [&'static str]),
    /// A CSS `font-family` list.
    Font,
    /// A `data:image/...;base64,` URL.
    Image,
    /// `true` / `false`.
    Flag,
}

const KEYS: &[(&str, Kind)] = &[
    ("enabled", Kind::Flag),
    ("name", Kind::Font),
    ("mode", Kind::Choice(&["system", "light", "dark"])),
    // Chrome colors
    ("accent", Kind::ChromeColor("--accent")),
    ("tab_strip_bg", Kind::ChromeColor("--tab-strip-bg")),
    ("tab_inactive_bg", Kind::ChromeColor("--tab-inactive-bg")),
    ("tab_hover_bg", Kind::ChromeColor("--tab-hover-bg")),
    ("tab_active_bg", Kind::ChromeColor("--tab-active-bg")),
    ("toolbar_bg", Kind::ChromeColor("--toolbar-bg")),
    ("toolbar_border", Kind::ChromeColor("--toolbar-border")),
    ("omnibox_bg", Kind::ChromeColor("--omnibox-bg")),
    ("omnibox_focus_bg", Kind::ChromeColor("--omnibox-focus-bg")),
    ("text_primary", Kind::ChromeColor("--text-primary")),
    ("text_secondary", Kind::ChromeColor("--text-secondary")),
    ("btn_hover", Kind::ChromeColor("--btn-hover")),
    ("btn_active", Kind::ChromeColor("--btn-active")),
    ("divider", Kind::ChromeColor("--divider")),
    ("dropdown_bg", Kind::ChromeColor("--dropdown-bg")),
    ("dropdown_border", Kind::ChromeColor("--dropdown-border")),
    ("badge_on_bg", Kind::ChromeColor("--badge-on-bg")),
    ("badge_on_text", Kind::ChromeColor("--badge-on-text")),
    ("badge_off_bg", Kind::ChromeColor("--badge-off-bg")),
    ("badge_off_text", Kind::ChromeColor("--badge-off-text")),
    // Internal page colors
    ("page_bg", Kind::PageColor("--bg")),
    ("page_surface", Kind::PageColor("--surface")),
    ("page_surface_hover", Kind::PageColor("--surface-hover")),
    ("page_border", Kind::PageColor("--border")),
    ("page_text", Kind::PageColor("--text")),
    ("page_muted", Kind::PageColor("--muted")),
    ("page_accent", Kind::PageColor("--accent")),
    ("page_accent_hover", Kind::PageColor("--accent-hover")),
    // Shape & layout
    ("radius_tab", Kind::Number(0.0, 20.0)),
    ("radius_omnibox", Kind::Number(0.0, 20.0)),
    ("radius_button", Kind::Number(0.0, 16.0)),
    ("radius_menu", Kind::Number(0.0, 24.0)),
    ("tab_max_width", Kind::Number(100.0, 400.0)),
    (
        "tab_style",
        Kind::Choice(&["classic", "pill", "underline", "flat"]),
    ),
    ("active_glow", Kind::Flag),
    ("glow_color", Kind::Color),
    ("anim_scale", Kind::Number(0.0, 3.0)),
    // Typography
    ("font_ui", Kind::Font),
    ("font_size_tab", Kind::Number(9.0, 18.0)),
    ("font_size_omnibox", Kind::Number(10.0, 20.0)),
    ("font_size_menu", Kind::Number(10.0, 18.0)),
    ("font_weight", Kind::Number(300.0, 700.0)),
    // Backgrounds
    ("bg_mode", Kind::Choice(&["none", "gradient", "image"])),
    ("bg_target", Kind::Choice(&["both", "chrome", "pages"])),
    (
        "gradient_kind",
        Kind::Choice(&["linear", "radial", "conic"]),
    ),
    ("gradient_angle", Kind::Number(0.0, 360.0)),
    ("gradient_from", Kind::Color),
    ("gradient_via", Kind::Color),
    ("gradient_to", Kind::Color),
    ("image", Kind::Image),
    ("image_fit", Kind::Choice(&["cover", "contain", "tile"])),
    ("bg_strength", Kind::Number(0.0, 100.0)),
    ("page_blur", Kind::Number(0.0, 30.0)),
];

fn kind_of(key: &str) -> Option<Kind> {
    KEYS.iter().find(|(k, _)| *k == key).map(|(_, kind)| *kind)
}

/// Returns `true` if `key` is a known theme key.
pub fn is_known_key(key: &str) -> bool {
    kind_of(key).is_some()
}

/// Validates a CSS color: `#rgb[a]`, `#rrggbb[aa]`, `rgb[a](...)`, `hsl[a](...)`, `transparent`.
pub fn is_valid_color(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    if v.eq_ignore_ascii_case("transparent") {
        return true;
    }
    if let Some(hex) = v.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    let lower = v.to_ascii_lowercase();
    let Some(open) = lower.find('(') else {
        return false;
    };
    if !matches!(&lower[..open], "rgb" | "rgba" | "hsl" | "hsla") || !lower.ends_with(')') {
        return false;
    }
    let inner = &lower[open + 1..lower.len() - 1];
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '%' | '/' | '-'))
        && !lower.contains("deg(")
}

fn is_valid_font(v: &str) -> bool {
    let v = v.trim();
    !v.is_empty()
        && v.len() <= 120
        && v.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ' ' | ',' | '-' | '_' | '"' | '\'' | '.')
        })
}

fn is_valid_image(v: &str) -> bool {
    if v.len() > MAX_IMAGE_DATA_URL_LEN {
        return false;
    }
    let Some(rest) = v.strip_prefix("data:image/") else {
        return false;
    };
    let Some((mime, data)) = rest.split_once(";base64,") else {
        return false;
    };
    matches!(mime, "png" | "jpeg" | "jpg" | "webp" | "gif" | "avif")
        && !data.is_empty()
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Validates and normalizes `value` for `key`. Returns `None` if the key is unknown or the
/// value is not acceptable.
pub fn normalize_value(key: &str, value: &str) -> Option<String> {
    let value = value.trim();
    match kind_of(key)? {
        Kind::ChromeColor(_) | Kind::PageColor(_) | Kind::Color => {
            is_valid_color(value).then(|| value.to_owned())
        }
        Kind::Number(min, max) => {
            let n = value.parse::<f64>().ok().filter(|n| n.is_finite())?;
            let n = n.clamp(min, max);
            let rounded = (n * 100.0).round() / 100.0;
            Some(format!("{rounded}"))
        }
        Kind::Choice(options) => options
            .iter()
            .find(|o| o.eq_ignore_ascii_case(value))
            .map(|o| (*o).to_owned()),
        Kind::Font => is_valid_font(value).then(|| value.to_owned()),
        Kind::Image => is_valid_image(value).then(|| value.to_owned()),
        Kind::Flag => match value {
            "true" | "1" | "on" => Some(String::from("true")),
            "false" | "0" | "off" => Some(String::from("false")),
            _ => None,
        },
    }
}

/// A validated custom theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CustomTheme {
    values: BTreeMap<String, String>,
}

impl CustomTheme {
    /// Parses `key=value` lines (unknown keys and invalid values are skipped).
    pub fn parse(text: &str) -> Self {
        let mut theme = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                theme.set(k.trim(), v);
            }
        }
        theme
    }

    /// Serializes to `key=value` lines.
    pub fn serialize(&self) -> String {
        let mut out = String::from("# Ember custom theme (edit at ember://themes)\n");
        for (k, v) in &self.values {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        out
    }

    /// Sets a key if the value is valid; an empty value removes the key. Returns `true` if the
    /// key was accepted (set or removed).
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        if value.trim().is_empty() {
            return self.values.remove(key).is_some() || is_known_key(key);
        }
        match normalize_value(key, value) {
            Some(v) => {
                self.values.insert(key.to_owned(), v);
                true
            }
            None => false,
        }
    }

    /// Iterates over all stored `(key, value)` pairs in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Returns the stored value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Whether the custom theme is switched on.
    pub fn enabled(&self) -> bool {
        self.get("enabled") == Some("true")
    }

    /// The theme's preferred light/dark mode, if any.
    pub fn mode(&self) -> Option<&str> {
        self.get("mode")
    }

    fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|v| v.parse::<f64>().ok())
    }

    fn background_image_css(&self) -> Option<String> {
        match self.get("bg_mode")? {
            "gradient" => {
                let from = self.get("gradient_from")?;
                let to = self.get("gradient_to").unwrap_or(from);
                let stops = match self.get("gradient_via") {
                    Some(via) => format!("{from}, {via}, {to}"),
                    None => format!("{from}, {to}"),
                };
                let angle = self.num("gradient_angle").unwrap_or(135.0);
                Some(match self.get("gradient_kind").unwrap_or("linear") {
                    "radial" => format!("radial-gradient(circle at 30% 20%, {stops})"),
                    "conic" => format!("conic-gradient(from {angle}deg at 50% 50%, {stops})"),
                    _ => format!("linear-gradient({angle}deg, {stops})"),
                })
            }
            "image" => {
                let img = self.get("image")?;
                Some(format!("url(\"{img}\")"))
            }
            _ => None,
        }
    }

    fn background_size_css(&self) -> &'static str {
        if self.get("bg_mode") != Some("image") {
            return "cover";
        }
        match self.get("image_fit").unwrap_or("cover") {
            "contain" => "contain",
            "tile" => "auto",
            _ => "cover",
        }
    }

    fn background_repeat_css(&self) -> &'static str {
        if self.get("bg_mode") == Some("image") && self.get("image_fit") == Some("tile") {
            "repeat"
        } else {
            "no-repeat"
        }
    }

    /// Percentage of the theme's own surface colors kept on top of the background
    /// (100 - bg_strength). 0 means the background shows through fully.
    fn surface_keep_pct(&self) -> f64 {
        100.0 - self.num("bg_strength").unwrap_or(60.0)
    }

    /// CSS injected into the browser chrome (`<style id="ember-custom-theme">`).
    /// Empty when the theme is disabled.
    pub fn chrome_css(&self) -> String {
        if !self.enabled() {
            return String::new();
        }
        let mut vars = String::new();
        for (key, kind) in KEYS {
            if let (Kind::ChromeColor(var), Some(v)) = (kind, self.get(key)) {
                vars.push_str(&format!("{var}:{v} !important;"));
            }
        }
        let px_vars = [
            ("radius_tab", "--radius-tab"),
            ("radius_omnibox", "--radius-omnibox"),
            ("radius_button", "--radius-button"),
            ("radius_menu", "--radius-menu"),
            ("tab_max_width", "--tab-max-width"),
            ("font_size_tab", "--font-size-tab"),
            ("font_size_omnibox", "--font-size-omnibox"),
            ("font_size_menu", "--font-size-menu"),
        ];
        for (key, var) in px_vars {
            if let Some(n) = self.num(key) {
                vars.push_str(&format!("{var}:{n}px !important;"));
            }
        }
        if let Some(font) = self.get("font_ui") {
            vars.push_str(&format!("--font-ui:{font} !important;"));
        }
        if let Some(scale) = self.num("anim_scale") {
            vars.push_str(&format!(
                "--tab-anim-duration:{}ms !important;--btn-anim-duration:{}ms !important;",
                (180.0 * scale).round(),
                (120.0 * scale).round()
            ));
        }

        let mut css = format!("html:root{{{vars}}}");
        if let Some(w) = self.num("font_weight") {
            css.push_str(&format!(
                ".tab-title,.omnibox-input,.menu-item{{font-weight:{w} !important}}"
            ));
        }
        if self.num("radius_button").is_some() {
            css.push_str(
                ".nav-btn,.omnibox-icon-btn{border-radius:var(--radius-button) !important}",
            );
        }

        match self.get("tab_style").unwrap_or("classic") {
            "pill" => css.push_str(
                ".tab-strip{align-items:center !important}\
                 .tabs-container{align-items:center !important}\
                 .tab{height:28px !important;border-radius:var(--radius-tab,14px) !important;margin-bottom:2px}\
                 .tab.active::before{display:none}\
                 .tab:not(.active)::after{display:none}",
            ),
            "underline" => css.push_str(
                ".tab,.tab.active{background:transparent !important;border-radius:0 !important}\
                 .tab:hover{background:var(--tab-hover-bg) !important}\
                 .tab.active::before{top:auto !important;bottom:0;left:4px !important;right:4px !important;height:3px !important}\
                 .tab.active{color:var(--accent) !important}",
            ),
            "flat" => css.push_str(
                ".tab{border-radius:0 !important}\
                 .tab.active::before{display:none}\
                 .tab:not(.active)::after{display:none}",
            ),
            _ => {}
        }

        if self.get("active_glow") == Some("true") {
            let glow = self.get("glow_color").unwrap_or("var(--accent)");
            css.push_str(&format!(
                ".tab.active{{box-shadow:0 0 12px 1px {glow}, inset 0 0 0 1px {glow} !important}}\
                 .omnibox-wrapper:focus-within{{box-shadow:0 0 14px 1px {glow} !important}}"
            ));
        }

        let target = self.get("bg_target").unwrap_or("both");
        if target != "pages" {
            if let Some(bg) = self.background_image_css() {
                let keep = self.surface_keep_pct();
                css.push_str(&format!(
                    ".chrome-shell{{background-image:{bg} !important;background-size:{size} !important;\
                     background-repeat:{rep} !important;background-position:center !important}}\
                     .tab-strip{{background:color-mix(in srgb, var(--tab-strip-bg) {keep}%, transparent) !important}}\
                     .toolbar{{background:color-mix(in srgb, var(--toolbar-bg) {keep}%, transparent) !important}}",
                    size = self.background_size_css(),
                    rep = self.background_repeat_css(),
                ));
            }
        }
        css
    }

    /// CSS appended to internal `ember://` pages. Empty when the theme is disabled.
    pub fn page_css(&self) -> String {
        if !self.enabled() {
            return String::new();
        }
        let mut vars = String::new();
        for (key, kind) in KEYS {
            if let (Kind::PageColor(var), Some(v)) = (kind, self.get(key)) {
                vars.push_str(&format!("{var}:{v} !important;"));
            }
        }
        // Fall back to the chrome accent for pages when no page accent is set.
        if self.get("page_accent").is_none() {
            if let Some(accent) = self.get("accent") {
                vars.push_str(&format!("--accent:{accent} !important;"));
            }
        }
        if let Some(font) = self.get("font_ui") {
            vars.push_str(&format!("--font-ui:{font} !important;"));
        }
        let mut css = format!("html:root{{{vars}}}");

        let target = self.get("bg_target").unwrap_or("both");
        if target != "chrome" {
            if let Some(bg) = self.background_image_css() {
                let keep = self.surface_keep_pct();
                let blur = self.num("page_blur").unwrap_or(0.0);
                css.push_str(&format!(
                    "html,body{{background:transparent !important}}\
                     body::before{{content:\"\";position:fixed;inset:-40px;z-index:-2;background-image:{bg};\
                     background-size:{size};background-repeat:{rep};background-position:center;filter:blur({blur}px)}}\
                     body::after{{content:\"\";position:fixed;inset:0;z-index:-1;\
                     background:color-mix(in srgb, var(--bg) {keep}%, transparent)}}",
                    size = self.background_size_css(),
                    rep = self.background_repeat_css(),
                ));
            }
        }
        css
    }
}

/// Inserts `css` as `<style id="ember-custom-theme">` before `</head>` of `html`.
pub fn inject_style(html: &str, css: &str) -> String {
    let tag = format!("<style id=\"ember-custom-theme\">{css}</style>");
    match html.find("</head>") {
        Some(pos) => {
            let mut out = String::with_capacity(html.len() + tag.len());
            out.push_str(&html[..pos]);
            out.push_str(&tag);
            out.push_str(&html[pos..]);
            out
        }
        None => format!("{tag}{html}"),
    }
}

/// Parses an editor payload (`key=value` lines) into a theme.
pub fn parse_payload(payload: &str) -> CustomTheme {
    CustomTheme::parse(payload)
}

struct ThemeState {
    path: Option<PathBuf>,
    theme: CustomTheme,
}

fn state() -> &'static Mutex<ThemeState> {
    static STATE: OnceLock<Mutex<ThemeState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(ThemeState {
            path: None,
            theme: CustomTheme::default(),
        })
    })
}

/// Loads `theme.txt` from `config_dir` into the process-wide theme.
pub fn init(config_dir: &Path) {
    let path = config_dir.join("theme.txt");
    let theme = fs::read_to_string(&path)
        .map(|t| CustomTheme::parse(&t))
        .unwrap_or_default();
    if let Ok(mut s) = state().lock() {
        s.path = Some(path);
        s.theme = theme;
    }
}

/// Returns a copy of the current process-wide theme.
pub fn current() -> CustomTheme {
    state().lock().map(|s| s.theme.clone()).unwrap_or_default()
}

/// Replaces the current theme in memory (used for live preview).
pub fn set_current(theme: CustomTheme) {
    if let Ok(mut s) = state().lock() {
        s.theme = theme;
    }
}

/// Replaces the current theme and writes it to disk.
pub fn save(theme: CustomTheme) -> io::Result<()> {
    let path = {
        let mut s = state()
            .lock()
            .map_err(|_| io::Error::other("theme state poisoned"))?;
        s.theme = theme.clone();
        s.path.clone()
    };
    if let Some(path) = path {
        let tmp = path.with_extension("txt.tmp");
        fs::write(&tmp, theme.serialize())?;
        fs::rename(&tmp, &path)?;
    }
    Ok(())
}

/// Reloads the last saved theme from disk (discarding an unsaved preview).
pub fn reload_saved() -> CustomTheme {
    let path = state().lock().ok().and_then(|s| s.path.clone());
    let theme = path
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| CustomTheme::parse(&t))
        .unwrap_or_default();
    set_current(theme.clone());
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_strictly_validated() {
        for ok in [
            "#fff",
            "#ffffff80",
            "rgb(1, 2, 3)",
            "rgba(1,2,3,0.5)",
            "hsl(200 50% 40%)",
            "transparent",
        ] {
            assert!(is_valid_color(ok), "{ok}");
        }
        for bad in [
            "red;}body{display:none",
            "#ggg",
            "url(x)",
            "rgb(1,2,3);x:y",
            "rgb(1,2,3)</style>",
            "expression(alert(1))",
            "",
        ] {
            assert!(!is_valid_color(bad), "{bad}");
        }
    }

    #[test]
    fn fonts_and_images_reject_breakouts() {
        assert!(is_valid_font("\"Fira Code\", monospace"));
        assert!(!is_valid_font("x;}</style><script>"));
        assert!(is_valid_image("data:image/png;base64,iVBORw0KGgo="));
        assert!(!is_valid_image("data:image/svg+xml;base64,PHN2Zz4="));
        assert!(!is_valid_image("data:image/png;base64,abc\")}body{x:y"));
        assert!(!is_valid_image("https://example.com/a.png"));
    }

    #[test]
    fn numbers_are_clamped_and_choices_checked() {
        assert_eq!(normalize_value("radius_tab", "99").as_deref(), Some("20"));
        assert_eq!(normalize_value("radius_tab", "-5").as_deref(), Some("0"));
        assert_eq!(normalize_value("radius_tab", "NaN"), None);
        assert_eq!(
            normalize_value("tab_style", "PILL").as_deref(),
            Some("pill")
        );
        assert_eq!(normalize_value("tab_style", "weird"), None);
        assert_eq!(normalize_value("nope", "1"), None);
    }

    #[test]
    fn roundtrip_and_disabled_theme_emits_nothing() {
        let mut t = CustomTheme::default();
        assert!(t.set("accent", "#ff00aa"));
        assert!(t.set("radius_tab", "12"));
        assert!(!t.set("accent", "javascript:alert(1)"));
        let back = CustomTheme::parse(&t.serialize());
        assert_eq!(back, t);
        assert!(t.chrome_css().is_empty());
        assert!(t.page_css().is_empty());
        t.set("enabled", "true");
        let css = t.chrome_css();
        assert!(css.contains("--accent:#ff00aa !important"));
        assert!(css.contains("--radius-tab:12px"));
        assert!(t.page_css().contains("--accent:#ff00aa"));
    }

    #[test]
    fn gradient_and_image_backgrounds() {
        let mut t = CustomTheme::parse(
            "enabled=true\nbg_mode=gradient\ngradient_from=#000\ngradient_to=#fff\ngradient_angle=90\nbg_strength=40",
        );
        assert!(t
            .chrome_css()
            .contains("linear-gradient(90deg, #000, #fff)"));
        assert!(t.chrome_css().contains("60%"));
        assert!(t.page_css().contains("body::before"));
        t.set("bg_target", "chrome");
        assert!(!t.page_css().contains("body::before"));
        t.set("bg_mode", "image");
        t.set("image", "data:image/png;base64,AAAA");
        t.set("image_fit", "tile");
        let css = t.chrome_css();
        assert!(css.contains("url(\"data:image/png;base64,AAAA\")"));
        assert!(css.contains("background-repeat:repeat"));
    }

    #[test]
    fn empty_value_removes_key_and_inject_style_works() {
        let mut t = CustomTheme::parse("accent=#123456");
        assert!(t.set("accent", ""));
        assert_eq!(t.get("accent"), None);
        let html = inject_style("<html><head></head><body></body></html>", "a{b:c}");
        assert!(html.contains("<style id=\"ember-custom-theme\">a{b:c}</style></head>"));
    }
}
