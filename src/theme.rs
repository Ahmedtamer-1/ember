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
use std::io::{self, Read, Seek, SeekFrom, Write};
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
    (
        "bg_mode",
        Kind::Choice(&["none", "gradient", "image", "animated", "video"]),
    ),
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
    // Animated wallpapers
    (
        "anim_style",
        Kind::Choice(&["flow", "hue", "pulse", "aurora"]),
    ),
    ("anim_speed", Kind::Number(0.1, 5.0)),
    ("video", Kind::Choice(VIDEO_EXTS)),
    (
        "particles",
        Kind::Choice(&["none", "snow", "stars", "fireflies", "rain"]),
    ),
    ("particle_count", Kind::Number(5.0, 150.0)),
    ("particle_speed", Kind::Number(0.2, 3.0)),
    ("particle_color", Kind::Color),
    ("animate_focus_only", Kind::Flag),
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
    matches!(
        mime,
        "png" | "apng" | "jpeg" | "jpg" | "webp" | "gif" | "avif"
    ) && !data.is_empty()
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

        if self.bg_target_has("chrome") {
            css.push_str(&self.chrome_background_css());
        }
        css
    }

    /// Whether the background applies to `side` (`"chrome"` or `"pages"`).
    fn bg_target_has(&self, side: &str) -> bool {
        match self.get("bg_target").unwrap_or("both") {
            "both" => true,
            t => t == side,
        }
    }

    /// Background rules for the chrome (`.chrome-shell` / `.ember-wp` layer).
    fn chrome_background_css(&self) -> String {
        let keep = self.surface_keep_pct();
        let see_through = format!(
            ".tab-strip{{background:color-mix(in srgb, var(--tab-strip-bg) {keep}%, transparent) !important}}\
             .toolbar{{background:color-mix(in srgb, var(--toolbar-bg) {keep}%, transparent) !important}}"
        );
        match self.get("bg_mode") {
            Some("gradient" | "image") => match self.background_image_css() {
                Some(bg) => format!(
                    ".chrome-shell{{background-image:{bg} !important;background-size:{size} !important;\
                     background-repeat:{rep} !important;background-position:center !important}}{see_through}",
                    size = self.background_size_css(),
                    rep = self.background_repeat_css(),
                ),
                None => String::new(),
            },
            Some("animated") => format!(
                ".ember-wp{{display:block !important}}{}{see_through}",
                self.animated_layers_css(".ember-wp::before", ".ember-wp::after", "absolute", "")
            ),
            Some("video") if self.get("video").is_some() => format!(
                ".ember-wp{{display:block !important}}\
                 .ember-wp video{{position:absolute;inset:0;width:100%;height:100%;object-fit:cover}}{see_through}"
            ),
            _ => String::new(),
        }
    }

    /// GPU-friendly animated gradient layers: only `transform`, `opacity` and `filter` are
    /// animated, on pseudo-element layers, so the compositor runs them without repainting.
    /// `base` is the bottom layer, `blob` the one above it; `extra` is appended to both rules.
    /// Play state follows `--ember-wp-play` (set by the wallpaper runtime).
    fn animated_layers_css(&self, base: &str, blob: &str, pos: &str, extra: &str) -> String {
        let from = self.get("gradient_from").unwrap_or("#3a1c71");
        let via = self.get("gradient_via").unwrap_or("#d76d77");
        let to = self.get("gradient_to").unwrap_or("#ffaf7b");
        let angle = self.num("gradient_angle").unwrap_or(135.0);
        let speed = self.num("anim_speed").unwrap_or(1.0).max(0.1);
        let dur = |secs: f64| (secs / speed * 10.0).round() / 10.0;
        let layer = format!("content:\"\";position:{pos};pointer-events:none;{extra}");
        let play = "animation-play-state:var(--ember-wp-play,running)";
        let mut css = match self.get("anim_style").unwrap_or("flow") {
            "hue" => format!(
                "{base}{{{layer}inset:0;background:linear-gradient({angle}deg,{from},{via},{to});\
                 will-change:filter;animation:ember-wp-hue {d}s linear infinite;{play}}}\
                 @keyframes ember-wp-hue{{from{{filter:hue-rotate(0deg)}}to{{filter:hue-rotate(360deg)}}}}",
                d = dur(24.0)
            ),
            "pulse" => format!(
                "{base}{{{layer}inset:0;background:linear-gradient({angle}deg,{from},{to})}}\
                 {blob}{{{layer}inset:-15%;background:radial-gradient(closest-side,{via},transparent);\
                 will-change:transform,opacity;animation:ember-wp-pulse {d}s ease-in-out infinite alternate;{play}}}\
                 @keyframes ember-wp-pulse{{from{{opacity:.3;transform:scale(.85)}}to{{opacity:1;transform:scale(1.1)}}}}",
                d = dur(8.0)
            ),
            "aurora" => format!(
                "{base}{{{layer}inset:0;background:linear-gradient({angle}deg,{from},{to})}}\
                 {blob}{{{layer}inset:-20%;opacity:.85;\
                 background:radial-gradient(40% 60% at 25% 35%,{via},transparent 70%),\
                 radial-gradient(35% 55% at 70% 60%,{to},transparent 70%),\
                 radial-gradient(30% 50% at 50% 20%,{from},transparent 70%);\
                 will-change:transform;animation:ember-wp-aurora {d}s ease-in-out infinite alternate;{play}}}\
                 @keyframes ember-wp-aurora{{0%{{transform:translate3d(-10%,3%,0) rotate(-5deg) scale(1)}}\
                 50%{{transform:translate3d(3%,-5%,0) rotate(4deg) scale(1.12)}}\
                 100%{{transform:translate3d(10%,2%,0) rotate(-2deg) scale(1.04)}}}}",
                d = dur(26.0)
            ),
            _ => format!(
                "{base}{{{layer}inset:-25%;background:linear-gradient({angle}deg,{from},{via},{to},{via},{from});\
                 will-change:transform;animation:ember-wp-flow {d}s ease-in-out infinite alternate;{play}}}\
                 @keyframes ember-wp-flow{{from{{transform:translate3d(-12%,-8%,0)}}to{{transform:translate3d(12%,8%,0)}}}}",
                d = dur(18.0)
            ),
        };
        css.push_str(&format!(
            "@media (prefers-reduced-motion: reduce){{{base},{blob}{{animation:none !important}}}}"
        ));
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

        if self.bg_target_has("pages") {
            css.push_str(&self.page_background_css());
        }
        css
    }

    /// Background rules for internal pages (fixed layers behind `body`).
    fn page_background_css(&self) -> String {
        let keep = self.surface_keep_pct();
        let blur = self.num("page_blur").unwrap_or(0.0);
        let clear = "html,body{background:transparent !important}";
        let overlay = format!(
            "body::after{{content:\"\";position:fixed;inset:0;z-index:-1;\
             background:color-mix(in srgb, var(--bg) {keep}%, transparent)}}"
        );
        match self.get("bg_mode") {
            Some("gradient" | "image") => match self.background_image_css() {
                Some(bg) => format!(
                    "{clear}body::before{{content:\"\";position:fixed;inset:-40px;z-index:-2;background-image:{bg};\
                     background-size:{size};background-repeat:{rep};background-position:center;filter:blur({blur}px)}}{overlay}",
                    size = self.background_size_css(),
                    rep = self.background_repeat_css(),
                ),
                None => String::new(),
            },
            Some("animated") => format!(
                "{clear}{}{overlay}",
                self.animated_layers_css("html::before", "body::before", "fixed", "z-index:-2;")
            ),
            Some("video") if self.get("video").is_some() => {
                let filter = if blur > 0.0 {
                    format!("filter:blur({blur}px);")
                } else {
                    String::new()
                };
                format!(
                    "{clear}#ember-wp{{position:fixed;inset:-{m}px;z-index:-2;pointer-events:none;overflow:hidden;{filter}}}\
                     #ember-wp video{{position:absolute;inset:0;width:100%;height:100%;object-fit:cover}}{overlay}",
                    m = (blur * 2.0).round()
                )
            }
            _ => String::new(),
        }
    }

    /// Whether the animated wallpaper runtime has anything to do on `side`
    /// (`"chrome"` or `"pages"`): an animated gradient, a video or particles.
    pub fn wallpaper_active(&self, side: &str) -> bool {
        if !self.enabled() {
            return false;
        }
        let bg = self.bg_target_has(side)
            && match self.get("bg_mode") {
                Some("animated") => true,
                Some("video") => self.get("video").is_some(),
                _ => false,
            };
        let particles = side == "chrome" && !matches!(self.get("particles"), None | Some("none"));
        bg || particles
    }

    /// JSON config for the wallpaper runtime (`window.__emberSetWallpaper`). `video_url` is
    /// the served wallpaper video (see [`wallpaper_video_url`]); `reduced` forces still mode.
    /// Every string in it is a validated theme value or the Rust-built video URL.
    pub fn wallpaper_config_json(
        &self,
        side: &str,
        video_url: Option<&str>,
        reduced: bool,
    ) -> String {
        let active = self.wallpaper_active(side);
        let video = video_url
            .filter(|_| active && self.bg_target_has(side) && self.get("bg_mode") == Some("video"));
        let particles = match self.get("particles") {
            Some(p) if active && side == "chrome" && p != "none" => Some(p),
            _ => None,
        };
        let mut out = format!(
            "{{\"active\":{active},\"focus_only\":{},\"reduced\":{reduced},\"video\":",
            self.get("animate_focus_only") != Some("false")
        );
        push_json_opt(&mut out, video);
        out.push_str(",\"particles\":");
        push_json_opt(&mut out, particles);
        out.push_str(&format!(
            ",\"count\":{},\"pspeed\":{},\"pcolor\":",
            self.num("particle_count").unwrap_or(60.0),
            self.num("particle_speed").unwrap_or(1.0)
        ));
        push_json_opt(
            &mut out,
            Some(self.get("particle_color").unwrap_or("#ffffff")),
        );
        out.push('}');
        out
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

/// Appends a JSON string (or `null`) to `out`, escaping quotes, backslashes, `<`, `>` and
/// control characters so the result is also safe inside a `<script>` element.
fn push_json_opt(out: &mut String, value: Option<&str>) {
    let Some(v) = value else {
        out.push_str("null");
        return;
    };
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The wallpaper runtime: video layer, particle canvas and pause/resume logic shared by the
/// chrome and internal pages. Idle (no timers, no listeners) until a config activates it.
pub const WALLPAPER_RUNTIME_JS: &str = include_str!("wallpaper_runtime.js");

/// Inserts the wallpaper runtime and its initial `config_json` before `</body>` of `html`.
pub fn inject_wallpaper_runtime(html: &str, config_json: &str) -> String {
    let tag = format!(
        "<script>window.__emberWallpaperInit={config_json};</script><script>{WALLPAPER_RUNTIME_JS}</script>"
    );
    match html.rfind("</body>") {
        Some(pos) => {
            let mut out = String::with_capacity(html.len() + tag.len());
            out.push_str(&html[..pos]);
            out.push_str(&tag);
            out.push_str(&html[pos..]);
            out
        }
        None => format!("{html}{tag}"),
    }
}

/// Wallpaper runtime config for the chrome, from the current theme.
pub fn chrome_wallpaper_json() -> String {
    current().wallpaper_config_json("chrome", wallpaper_video_url().as_deref(), false)
}

/// Wallpaper runtime config for internal pages, from the current theme.
pub fn page_wallpaper_json(reduced: bool) -> String {
    current().wallpaper_config_json("pages", wallpaper_video_url().as_deref(), reduced)
}

// ---------------------------------------------------------------------------------------
// Video wallpapers
//
// The editor reads the picked file and sends it in base64 chunks over privileged IPC
// (`wallpaper_begin:` / `wallpaper_chunk:` / `wallpaper_end`); each chunk is acknowledged
// before the next is sent, so at most one chunk is in flight. Rust validates the extension,
// size and container magic bytes and writes `wallpaper.<ext>` next to `theme.txt`.
// The chrome and internal pages then load it from the `emberwp` custom protocol
// (`http://emberwp.localhost/<token>/wallpaper.<ext>` on Windows), which serves HTTP range
// requests straight from disk on a worker thread. The random token keeps web pages from
// guessing the URL.
// ---------------------------------------------------------------------------------------

/// Largest accepted wallpaper video (bytes).
pub const MAX_VIDEO_BYTES: u64 = 50 * 1024 * 1024;
/// Largest decoded upload chunk (bytes).
pub const MAX_UPLOAD_CHUNK: usize = 1024 * 1024;
/// Accepted wallpaper video file extensions.
pub const VIDEO_EXTS: &[&str] = &["mp4", "webm"];
/// Name of the custom protocol that serves the wallpaper video.
pub const WALLPAPER_PROTOCOL: &str = "emberwp";
/// Largest body returned for one range request (bytes).
const MAX_SERVE_CHUNK: u64 = 2 * 1024 * 1024;

/// Returns the canonical extension if `ext` is an accepted video type (`mp4`, `webm`).
pub fn video_ext(ext: &str) -> Option<&'static str> {
    let ext = ext.trim();
    VIDEO_EXTS
        .iter()
        .find(|e| e.eq_ignore_ascii_case(ext))
        .copied()
}

/// Checks the container signature: MP4 has `ftyp` at offset 4, WebM starts with the EBML magic.
fn video_magic_ok(ext: &str, head: &[u8]) -> bool {
    match ext {
        "mp4" => head.get(4..8) == Some(&b"ftyp"[..]),
        "webm" => head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]),
        _ => false,
    }
}

/// Decodes standard base64 (padding optional). Returns `None` on any invalid character.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let data = input.trim().trim_end_matches('=').as_bytes();
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

fn upload_tmp_path(dir: &Path) -> PathBuf {
    dir.join("wallpaper.upload.tmp")
}

/// Returns the stored wallpaper video in `dir` and its extension, if any.
fn wallpaper_file_in(dir: &Path) -> Option<(PathBuf, &'static str)> {
    VIDEO_EXTS.iter().find_map(|ext| {
        let path = dir.join(format!("wallpaper.{ext}"));
        path.is_file().then_some((path, *ext))
    })
}

/// An in-progress chunked video upload.
struct VideoUpload {
    dir: PathBuf,
    file: fs::File,
    ext: &'static str,
    expected: u64,
    written: u64,
    next_seq: u64,
}

impl VideoUpload {
    fn begin(dir: &Path, ext: &str, size: u64) -> Result<Self, String> {
        let ext = video_ext(ext).ok_or("Only MP4 and WebM videos are supported")?;
        if size == 0 || size > MAX_VIDEO_BYTES {
            return Err(format!(
                "Video too large (max {} MB)",
                MAX_VIDEO_BYTES / (1024 * 1024)
            ));
        }
        let file = fs::File::create(upload_tmp_path(dir)).map_err(|e| e.to_string())?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            ext,
            expected: size,
            written: 0,
            next_seq: 0,
        })
    }

    fn chunk(&mut self, seq: u64, b64: &str) -> Result<u64, String> {
        if seq != self.next_seq {
            return Err(String::from("Upload chunk out of order"));
        }
        if b64.len() > MAX_UPLOAD_CHUNK.div_ceil(3) * 4 + 4 {
            return Err(String::from("Upload chunk too large"));
        }
        let data = decode_base64(b64).ok_or("Corrupt upload chunk")?;
        if data.is_empty() || data.len() > MAX_UPLOAD_CHUNK {
            return Err(String::from("Upload chunk too large"));
        }
        if self.written + data.len() as u64 > self.expected {
            return Err(String::from("Upload larger than announced"));
        }
        if self.written == 0 && !video_magic_ok(self.ext, &data) {
            return Err(String::from("Not a valid MP4 / WebM video file"));
        }
        self.file.write_all(&data).map_err(|e| e.to_string())?;
        self.written += data.len() as u64;
        self.next_seq += 1;
        Ok(self.written)
    }

    fn finish(self) -> Result<&'static str, String> {
        let Self {
            dir,
            mut file,
            ext,
            expected,
            written,
            ..
        } = self;
        let tmp = upload_tmp_path(&dir);
        if written != expected {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(String::from("Upload incomplete"));
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        for e in VIDEO_EXTS {
            let _ = fs::remove_file(dir.join(format!("wallpaper.{e}")));
        }
        fs::rename(&tmp, dir.join(format!("wallpaper.{ext}"))).map_err(|e| e.to_string())?;
        Ok(ext)
    }

    fn abort(self) {
        let tmp = upload_tmp_path(&self.dir);
        drop(self.file);
        let _ = fs::remove_file(tmp);
    }
}

fn upload_slot() -> &'static Mutex<Option<VideoUpload>> {
    static SLOT: OnceLock<Mutex<Option<VideoUpload>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn config_dir() -> Option<PathBuf> {
    let s = state().lock().ok()?;
    s.path.as_ref()?.parent().map(Path::to_path_buf)
}

/// Starts a wallpaper video upload of `size` bytes with extension `ext` (replacing any
/// unfinished one).
pub fn wallpaper_upload_begin(ext: &str, size: u64) -> Result<(), String> {
    let dir = config_dir().ok_or("No config directory")?;
    let mut slot = upload_slot().lock().map_err(|_| "Upload state poisoned")?;
    if let Some(old) = slot.take() {
        old.abort();
    }
    *slot = Some(VideoUpload::begin(&dir, ext, size)?);
    Ok(())
}

/// Appends base64 chunk number `seq`. Returns the total bytes written so far. Any error
/// aborts the upload.
pub fn wallpaper_upload_chunk(seq: u64, b64: &str) -> Result<u64, String> {
    let mut slot = upload_slot().lock().map_err(|_| "Upload state poisoned")?;
    let Some(upload) = slot.as_mut() else {
        return Err(String::from("No upload in progress"));
    };
    match upload.chunk(seq, b64) {
        Ok(n) => Ok(n),
        Err(e) => {
            if let Some(up) = slot.take() {
                up.abort();
            }
            Err(e)
        }
    }
}

/// Completes the upload and stores it as `wallpaper.<ext>`. Returns the extension.
pub fn wallpaper_upload_finish() -> Result<&'static str, String> {
    let mut slot = upload_slot().lock().map_err(|_| "Upload state poisoned")?;
    slot.take().ok_or("No upload in progress")?.finish()
}

/// Deletes the stored wallpaper video and any unfinished upload.
pub fn wallpaper_clear() {
    if let Ok(mut slot) = upload_slot().lock() {
        if let Some(up) = slot.take() {
            up.abort();
        }
    }
    if let Some(dir) = config_dir() {
        for e in VIDEO_EXTS {
            let _ = fs::remove_file(dir.join(format!("wallpaper.{e}")));
        }
    }
}

/// Per-process random path segment of wallpaper URLs.
fn wallpaper_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut out = String::with_capacity(32);
        for round in 0u64..2 {
            let mut h = RandomState::new().build_hasher();
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            h.write_u64(round);
            out.push_str(&format!("{:016x}", h.finish()));
        }
        out
    })
}

fn wallpaper_base_url() -> &'static str {
    if cfg!(any(target_os = "windows", target_os = "android")) {
        "http://emberwp.localhost"
    } else {
        "emberwp://localhost"
    }
}

/// URL of the stored wallpaper video (with a modification-time cache buster), if one exists.
pub fn wallpaper_video_url() -> Option<String> {
    let dir = config_dir()?;
    let (path, ext) = wallpaper_file_in(&dir)?;
    let version = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    Some(format!(
        "{}/{}/wallpaper.{ext}?v={version}",
        wallpaper_base_url(),
        wallpaper_token()
    ))
}

/// Parses the first range of a `Range: bytes=...` header into an inclusive `(start, end)`.
/// Returns `None` when the range is malformed or not satisfiable for `len` bytes.
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let first = spec.split(',').next()?.trim();
    let (a, b) = first.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let last = len.checked_sub(1)?;
    let (start, end) = if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        (len.saturating_sub(suffix), last)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() {
            last
        } else {
            b.parse::<u64>().ok()?.min(last)
        };
        (start, end)
    };
    (start <= end).then_some((start, end))
}

fn status_response(code: u16) -> wry::http::Response<Vec<u8>> {
    wry::http::Response::builder()
        .status(code)
        .body(Vec::new())
        .unwrap_or_default()
}

/// Builds the response for a wallpaper request `path` (`/<token>/wallpaper.<ext>`) with an
/// optional `Range` header, reading at most [`MAX_SERVE_CHUNK`] bytes from `dir`.
fn serve_wallpaper(
    dir: Option<&Path>,
    token: &str,
    path: &str,
    range: Option<&str>,
) -> wry::http::Response<Vec<u8>> {
    let Some(dir) = dir else {
        return status_response(404);
    };
    let Some(ext) = path
        .strip_prefix('/')
        .and_then(|p| p.strip_prefix(token))
        .and_then(|p| p.strip_prefix("/wallpaper."))
    else {
        return status_response(404);
    };
    let Some((file_path, ext)) = wallpaper_file_in(dir).filter(|(_, e)| *e == ext) else {
        return status_response(404);
    };
    let Ok(mut file) = fs::File::open(&file_path) else {
        return status_response(404);
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 || len > MAX_VIDEO_BYTES {
        return status_response(404);
    }
    let (status, start, end) = match range {
        Some(r) => match parse_range(r, len) {
            Some((s, e)) => (206, s, e.min(s + MAX_SERVE_CHUNK - 1)),
            None => {
                return wry::http::Response::builder()
                    .status(416)
                    .header("Content-Range", format!("bytes */{len}"))
                    .body(Vec::new())
                    .unwrap_or_default();
            }
        },
        None => (200, 0, len - 1),
    };
    let count = end - start + 1;
    let mut body = vec![0u8; count as usize];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut body).is_err() {
        return status_response(500);
    }
    let mime = if ext == "webm" {
        "video/webm"
    } else {
        "video/mp4"
    };
    let mut builder = wry::http::Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Accept-Ranges", "bytes")
        .header("Content-Length", count.to_string())
        .header("Cache-Control", "no-cache");
    if status == 206 {
        builder = builder.header("Content-Range", format!("bytes {start}-{end}/{len}"));
    }
    builder.body(body).unwrap_or_default()
}

/// Handler for the [`WALLPAPER_PROTOCOL`] custom protocol. Disk reads run on a worker
/// thread so the UI thread never blocks on I/O.
pub fn handle_wallpaper_protocol(
    _webview_id: wry::WebViewId<'_>,
    request: wry::http::Request<Vec<u8>>,
    responder: wry::RequestAsyncResponder,
) {
    let path = request.uri().path().to_owned();
    let range = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let dir = config_dir();
    let _ = std::thread::Builder::new()
        .name(String::from("ember-wallpaper-io"))
        .spawn(move || {
            responder.respond(serve_wallpaper(
                dir.as_deref(),
                wallpaper_token(),
                &path,
                range.as_deref(),
            ));
        });
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

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ember_theme_test_{tag}_{}_{}",
            std::process::id(),
            wallpaper_token()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn b64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= c.len() {
                    out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn mp4_bytes(len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        v[4..8].copy_from_slice(b"ftyp");
        v
    }

    #[test]
    fn animation_keys_are_validated() {
        assert_eq!(
            normalize_value("bg_mode", "Animated").as_deref(),
            Some("animated")
        );
        assert_eq!(
            normalize_value("bg_mode", "video").as_deref(),
            Some("video")
        );
        assert_eq!(
            normalize_value("anim_style", "aurora").as_deref(),
            Some("aurora")
        );
        assert_eq!(normalize_value("anim_style", "aurora;x"), None);
        assert_eq!(normalize_value("anim_speed", "99").as_deref(), Some("5"));
        assert_eq!(normalize_value("anim_speed", "0").as_deref(), Some("0.1"));
        assert_eq!(normalize_value("video", "WEBM").as_deref(), Some("webm"));
        assert_eq!(normalize_value("video", "mov"), None);
        assert_eq!(normalize_value("video", "mp4\";}"), None);
        assert_eq!(
            normalize_value("particles", "snow").as_deref(),
            Some("snow")
        );
        assert_eq!(normalize_value("particles", "lasers"), None);
        assert_eq!(
            normalize_value("particle_count", "1000").as_deref(),
            Some("150")
        );
        assert_eq!(normalize_value("particle_color", "red</style>"), None);
        assert_eq!(
            normalize_value("animate_focus_only", "off").as_deref(),
            Some("false")
        );
        // Animated images (GIF / animated WebP / APNG) stay accepted.
        assert!(is_valid_image("data:image/gif;base64,R0lGODlh"));
        assert!(is_valid_image("data:image/apng;base64,iVBORw0KGgo="));
        assert!(is_valid_image("data:image/webp;base64,UklGRg=="));
    }

    #[test]
    fn animated_gradient_css_is_composited_and_pausable() {
        let mut t = CustomTheme::parse(
            "enabled=true\nbg_mode=animated\nanim_style=flow\nanim_speed=2\ngradient_from=#112233\ngradient_to=#445566",
        );
        let css = t.chrome_css();
        assert!(css.contains(".ember-wp{display:block !important}"));
        assert!(css.contains(".ember-wp::before{"));
        assert!(css.contains("@keyframes ember-wp-flow"));
        assert!(css.contains("transform:translate3d"));
        assert!(css.contains("ember-wp-flow 9s"));
        assert!(css.contains("animation-play-state:var(--ember-wp-play,running)"));
        assert!(css.contains("prefers-reduced-motion: reduce"));
        assert!(css.contains("#112233"));
        assert!(css.contains("color-mix(in srgb, var(--toolbar-bg)"));
        let page = t.page_css();
        assert!(page.contains("html::before{"));
        assert!(page.contains("position:fixed"));
        assert!(page.contains("body::after"));

        for (style, needle) in [
            ("hue", "hue-rotate(360deg)"),
            ("pulse", "@keyframes ember-wp-pulse"),
            ("aurora", ".ember-wp::after{"),
        ] {
            t.set("anim_style", style);
            assert!(t.chrome_css().contains(needle), "{style}");
        }
        // Chrome-only target: pages get no layers.
        t.set("bg_target", "chrome");
        assert!(!t.page_css().contains("::before"));
        // Static modes never emit animation.
        t.set("bg_mode", "gradient");
        assert!(!t.chrome_css().contains("@keyframes"));
        t.set("enabled", "false");
        assert!(t.chrome_css().is_empty());
    }

    #[test]
    fn video_css_and_wallpaper_config() {
        let mut t = CustomTheme::parse("enabled=true\nbg_mode=video\nbg_strength=30");
        // No stored video: no layer, runtime inactive.
        assert!(!t.chrome_css().contains("ember-wp"));
        assert!(!t.wallpaper_active("chrome"));
        t.set("video", "webm");
        assert!(t.chrome_css().contains(".ember-wp video{"));
        assert!(t.chrome_css().contains("70%"));
        t.set("page_blur", "6");
        let page = t.page_css();
        assert!(page.contains("#ember-wp{position:fixed;inset:-12px"));
        assert!(page.contains("filter:blur(6px)"));

        let url = "http://emberwp.localhost/tok/wallpaper.webm?v=1";
        let json = t.wallpaper_config_json("chrome", Some(url), false);
        assert!(json.contains("\"active\":true"));
        assert!(json.contains("\"focus_only\":true"));
        assert!(json.contains(&format!("\"video\":\"{url}\"")));
        assert!(json.contains("\"particles\":null"));
        t.set("bg_target", "pages");
        assert!(t
            .wallpaper_config_json("chrome", Some(url), false)
            .contains("\"video\":null"));
        assert!(t
            .wallpaper_config_json("pages", Some(url), true)
            .contains("\"reduced\":true"));

        // Particles are chrome-only.
        let mut p = CustomTheme::parse(
            "enabled=true\nparticles=snow\nparticle_count=40\nparticle_color=#abcdef\nanimate_focus_only=false",
        );
        assert!(p.wallpaper_active("chrome"));
        assert!(!p.wallpaper_active("pages"));
        let json = p.wallpaper_config_json("chrome", None, false);
        assert!(json.contains("\"particles\":\"snow\""));
        assert!(json.contains("\"count\":40"));
        assert!(json.contains("\"pcolor\":\"#abcdef\""));
        assert!(json.contains("\"focus_only\":false"));
        assert!(p
            .wallpaper_config_json("pages", None, false)
            .contains("\"active\":false"));
        p.set("particles", "none");
        assert!(!p.wallpaper_active("chrome"));
        // Zero cost by default: a plain theme has nothing to animate.
        assert!(!CustomTheme::parse("enabled=true\naccent=#fff").wallpaper_active("chrome"));
    }

    #[test]
    fn json_strings_cannot_break_out_of_script() {
        let mut out = String::new();
        push_json_opt(&mut out, Some("a\"b\\c</script><x>\n"));
        assert_eq!(
            out,
            "\"a\\\"b\\\\c\\u003c/script\\u003e\\u003cx\\u003e\\u000a\""
        );
        let html =
            inject_wallpaper_runtime("<html><body><p></p></body></html>", "{\"active\":false}");
        let pos = html
            .find("__emberWallpaperInit={\"active\":false}")
            .expect("init");
        assert!(pos < html.find("</body>").expect("body"));
        assert!(html.contains("__emberSetWallpaper"));
    }

    #[test]
    fn base64_and_video_signatures() {
        assert_eq!(decode_base64("QUJD").as_deref(), Some(&b"ABC"[..]));
        assert_eq!(decode_base64("QUI=").as_deref(), Some(&b"AB"[..]));
        assert_eq!(decode_base64("QQ==").as_deref(), Some(&b"A"[..]));
        assert_eq!(decode_base64("QU!D"), None);
        assert_eq!(decode_base64("Q"), None);
        assert!(video_magic_ok("mp4", &mp4_bytes(16)));
        assert!(!video_magic_ok("mp4", b"\x1a\x45\xdf\xa3xxxxxxxx"));
        assert!(video_magic_ok("webm", b"\x1a\x45\xdf\xa3xxxx"));
        assert!(!video_magic_ok("webm", &mp4_bytes(16)));
        assert_eq!(video_ext("MP4"), Some("mp4"));
        assert_eq!(video_ext("mkv"), None);
        assert_eq!(video_ext("gif"), None);
    }

    #[test]
    fn video_upload_enforces_whitelist_cap_order_and_signature() {
        let dir = temp_dir("upload");
        assert!(VideoUpload::begin(&dir, "mov", 100).is_err());
        assert!(VideoUpload::begin(&dir, "exe", 100).is_err());
        assert!(VideoUpload::begin(&dir, "mp4", 0).is_err());
        assert!(VideoUpload::begin(&dir, "mp4", MAX_VIDEO_BYTES + 1).is_err());
        assert!(VideoUpload::begin(&dir, "mp4", MAX_VIDEO_BYTES).is_ok());

        // Wrong signature on the first chunk.
        let mut up = VideoUpload::begin(&dir, "webm", 16).expect("begin");
        assert!(up.chunk(0, &b64(&mp4_bytes(16))).is_err());
        up.abort();

        // Out-of-order and oversize chunks.
        let data = mp4_bytes(32);
        let mut up = VideoUpload::begin(&dir, "mp4", 32).expect("begin");
        assert!(up.chunk(1, &b64(&data[..16])).is_err());
        assert_eq!(up.chunk(0, &b64(&data[..16])), Ok(16));
        assert!(up.chunk(1, &b64(&mp4_bytes(17))).is_err());
        assert!(up.chunk(1, "not base64!").is_err());
        let big = "A".repeat(MAX_UPLOAD_CHUNK.div_ceil(3) * 4 + 8);
        assert!(up.chunk(1, &big).is_err());
        assert_eq!(up.chunk(1, &b64(&data[16..])), Ok(32));
        assert_eq!(up.finish(), Ok("mp4"));
        assert_eq!(fs::read(dir.join("wallpaper.mp4")).expect("stored"), data);
        assert!(!upload_tmp_path(&dir).exists());

        // A new upload of the other type replaces the old file; incomplete uploads fail.
        let mut up = VideoUpload::begin(&dir, "webm", 8).expect("begin");
        assert!(up.chunk(0, &b64(b"\x1a\x45\xdf\xa3")).is_ok());
        assert!(up.finish().is_err());
        assert!(dir.join("wallpaper.mp4").exists());
        let mut up = VideoUpload::begin(&dir, "webm", 4).expect("begin");
        assert!(up.chunk(0, &b64(b"\x1a\x45\xdf\xa3")).is_ok());
        assert_eq!(up.finish(), Ok("webm"));
        assert!(!dir.join("wallpaper.mp4").exists());
        assert_eq!(wallpaper_file_in(&dir).map(|(_, e)| e), Some("webm"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-500", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Some((0, 1)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=20-10", 100), None);
        assert_eq!(parse_range("bytes=-0", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(parse_range("bytes=0-", 0), None);
    }

    #[test]
    fn wallpaper_serving_checks_token_and_ranges() {
        let dir = temp_dir("serve");
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        fs::write(dir.join("wallpaper.mp4"), &data).expect("write");
        let ok = serve_wallpaper(
            Some(&dir),
            "tok",
            "/tok/wallpaper.mp4",
            Some("bytes=100-199"),
        );
        assert_eq!(ok.status(), 206);
        assert_eq!(ok.body().as_slice(), &data[100..200]);
        assert_eq!(
            ok.headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok()),
            Some("bytes 100-199/5000")
        );
        assert_eq!(
            ok.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("video/mp4")
        );
        let full = serve_wallpaper(Some(&dir), "tok", "/tok/wallpaper.mp4", None);
        assert_eq!(full.status(), 200);
        assert_eq!(full.body().len(), 5000);
        let bad_range =
            serve_wallpaper(Some(&dir), "tok", "/tok/wallpaper.mp4", Some("bytes=9000-"));
        assert_eq!(bad_range.status(), 416);
        for path in [
            "/wrong/wallpaper.mp4",
            "/tok/wallpaper.webm",
            "/tok/../theme.txt",
            "/tok/theme.txt",
            "/tokwallpaper.mp4",
        ] {
            assert_eq!(
                serve_wallpaper(Some(&dir), "tok", path, None).status(),
                404,
                "{path}"
            );
        }
        assert_eq!(
            serve_wallpaper(None, "tok", "/tok/wallpaper.mp4", None).status(),
            404
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
