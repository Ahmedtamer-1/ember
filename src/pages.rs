use crate::{
    adblock::Adblocker,
    settings::{BrowserSettings, SettingsStore},
    storage::{BookmarkEntry, HistoryEntry, Storage},
};

pub const NEWTAB_URL: &str = "ember://newtab";
pub const HISTORY_URL: &str = "ember://history";
pub const BOOKMARKS_URL: &str = "ember://bookmarks";
pub const DOWNLOADS_URL: &str = "ember://downloads";
pub const SETTINGS_URL: &str = "ember://settings";
pub const WARNING_URL: &str = "ember://warning";
pub const ADBLOCK_DEBUG_URL: &str = "ember://adblock-debug";

/// Services required to render dynamic internal pages.
pub struct PageCtx<'a> {
    pub storage: &'a Storage,
    pub settings: &'a SettingsStore,
    pub adblocker: &'a Adblocker,
}

/// Returns `true` if `url` is an internal `ember://` browser page.
pub fn is_internal_url(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("ember://")
}

/// Normalizes an `ember://` URL to its canonical form. Unknown paths fall back to the new tab page.
pub fn canonical_internal_url(url: &str) -> Option<&'static str> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("ember://")?;
    let path = rest
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    match path {
        "history" => Some(HISTORY_URL),
        "bookmarks" => Some(BOOKMARKS_URL),
        "downloads" => Some(DOWNLOADS_URL),
        "settings" => Some(SETTINGS_URL),
        "warning" => Some(WARNING_URL),
        "adblock-debug" => Some(ADBLOCK_DEBUG_URL),
        _ => Some(NEWTAB_URL),
    }
}

/// Returns the canonical URL if `url` is a singleton internal page that should be
/// opened in a new tab or focused if already open (`settings`, `history`, `bookmarks`, `downloads`).
pub fn is_singleton_internal_url(url: &str) -> Option<&'static str> {
    match canonical_internal_url(url)? {
        SETTINGS_URL => Some(SETTINGS_URL),
        HISTORY_URL => Some(HISTORY_URL),
        BOOKMARKS_URL => Some(BOOKMARKS_URL),
        DOWNLOADS_URL => Some(DOWNLOADS_URL),
        _ => None,
    }
}

/// Builds the `ember://warning` URL shown instead of an insecure HTTP page in HTTPS-Only mode.
pub fn warning_url_for(target: &str) -> String {
    format!("{WARNING_URL}?url={}", percent_encode(target))
}

/// Extracts the blocked target URL from an `ember://warning?url=...` URL.
pub fn warning_target(url: &str) -> Option<String> {
    let idx = url.find("?url=")?;
    let raw = url[idx + 5..].split('#').next().unwrap_or("");
    let decoded = percent_decode(raw);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

/// Renders an internal `ember://` page into `(page_title, html_document)`.
pub fn render_internal_page(url: &str, ctx: &PageCtx<'_>) -> Option<(String, String)> {
    let canonical = canonical_internal_url(url)?;
    match canonical {
        NEWTAB_URL => Some((
            String::from("New Tab"),
            render_newtab_html(&ctx.storage.most_visited(8)),
        )),
        HISTORY_URL => Some((
            String::from("History"),
            render_history_html(&ctx.storage.recent_history(200)),
        )),
        BOOKMARKS_URL => Some((
            String::from("Bookmarks"),
            render_bookmarks_html(&ctx.storage.list_bookmarks()),
        )),
        DOWNLOADS_URL => Some((
            String::from("Downloads"),
            render_downloads_html(ctx.storage.downloads_dir()),
        )),
        SETTINGS_URL => Some((
            String::from("Settings"),
            render_settings_html(
                &ctx.settings.get(),
                ctx.settings.config_path(),
                ctx.adblocker,
            ),
        )),
        WARNING_URL => Some((
            String::from("Connection not secure"),
            render_https_warning_html(&warning_target(url).unwrap_or_default()),
        )),
        ADBLOCK_DEBUG_URL => Some((
            String::from("Adblock Debug"),
            render_adblock_debug_html(ctx.adblocker),
        )),
        _ => None,
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
const SHARED_INTERNAL_CSS: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #f8fafc;
  --surface: #ffffff;
  --surface-hover: #f1f5f9;
  --border: #e2e8f0;
  --text: #0f172a;
  --muted: #64748b;
  --accent: #2563eb;
  --accent-hover: #1d4ed8;
  --danger: #dc2626;
  --shadow: 0 4px 20px rgba(15, 23, 42, 0.06);
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #18181b;
    --surface: #27272a;
    --surface-hover: #3f3f46;
    --border: #3f3f46;
    --text: #f4f4f5;
    --muted: #a1a1aa;
    --accent: #60a5fa;
    --accent-hover: #3b82f6;
    --danger: #f87171;
    --shadow: 0 4px 24px rgba(0, 0, 0, 0.35);
  }
}
* { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  background: var(--bg);
  color: var(--text);
  min-height: 100vh;
}
a { color: inherit; text-decoration: none; }
.nav-links {
  display: flex;
  gap: 10px;
}
.pill-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 7px 14px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  font-size: 13px;
  font-weight: 500;
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s;
}
.pill-btn:hover { background: var(--surface-hover); }
.pill-btn.danger { color: var(--danger); border-color: var(--danger); }
"#;

fn render_newtab_html(most_visited: &[HistoryEntry]) -> String {
    let default_sites: &[(&str, &str)] = &[
        ("Rust Programming Language", "https://www.rust-lang.org"),
        ("Crates.io Registry", "https://crates.io"),
        ("Docs.rs Documentation", "https://docs.rs"),
        ("MDN Web Docs", "https://developer.mozilla.org"),
        ("GitHub", "https://github.com"),
        ("Wikipedia", "https://www.wikipedia.org"),
    ];

    let mut tiles_html = String::new();
    if most_visited.is_empty() {
        for (title, url) in default_sites {
            tiles_html.push_str(&render_tile(title, url, None));
        }
    } else {
        for entry in most_visited {
            tiles_html.push_str(&render_tile(
                &entry.title,
                &entry.url,
                Some(entry.visit_count),
            ));
        }
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>New Tab</title>
<style>
{SHARED_INTERNAL_CSS}
.newtab-wrap {{
  max-width: 760px;
  margin: 0 auto;
  padding: 72px 24px 48px;
  display: flex;
  flex-direction: column;
  align-items: center;
}}
.top-bar {{
  width: 100%;
  display: flex;
  justify-content: flex-end;
  margin-bottom: 36px;
}}
.brand {{
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 28px;
}}
.brand-logo {{
  width: 44px;
  height: 44px;
  border-radius: 12px;
  background: linear-gradient(135deg, #f97316, #ef4444);
  display: flex;
  align-items: center;
  justify-content: center;
  color: #fff;
  font-weight: 700;
  font-size: 22px;
  box-shadow: var(--shadow);
}}
.brand h1 {{
  font-size: 32px;
  font-weight: 700;
  letter-spacing: -0.02em;
}}
.search-form {{
  width: 100%;
  max-width: 580px;
  margin-bottom: 42px;
  position: relative;
}}
.search-input {{
  width: 100%;
  height: 48px;
  padding: 0 48px 0 20px;
  border-radius: 24px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  font-size: 15px;
  box-shadow: var(--shadow);
  outline: none;
  transition: border-color 0.15s;
}}
.search-input:focus {{
  border-color: var(--accent);
}}
.section-label {{
  width: 100%;
  font-size: 12px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
  margin-bottom: 14px;
}}
.tiles-grid {{
  width: 100%;
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(165px, 1fr));
  gap: 14px;
}}
.tile-card {{
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 10px;
  padding: 16px;
  border-radius: 14px;
  border: 1px solid var(--border);
  background: var(--surface);
  cursor: pointer;
  transition: transform 0.12s, background 0.15s;
}}
.tile-card:hover {{
  background: var(--surface-hover);
  transform: translateY(-2px);
}}
.tile-icon {{
  width: 36px;
  height: 36px;
  border-radius: 10px;
  background: var(--surface-hover);
  color: var(--accent);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: 16px;
}}
.tile-title {{
  font-size: 13.5px;
  font-weight: 600;
  width: 100%;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.tile-meta {{
  font-size: 11.5px;
  color: var(--muted);
  width: 100%;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
</style>
</head>
<body>
<div class="newtab-wrap">
  <div class="top-bar">
    <div class="nav-links">
      <button class="pill-btn" onclick="go('ember://bookmarks')">&#9733; Bookmarks</button>
      <button class="pill-btn" onclick="go('ember://history')">History</button>
    </div>
  </div>
  <div class="brand">
    <div class="brand-logo">E</div>
    <h1>Ember</h1>
  </div>
  <form class="search-form" id="search-form">
    <input class="search-input" id="search-input" type="text" placeholder="Search Google or type a URL" autocomplete="off" autofocus />
  </form>
  <div class="section-label">Most Visited</div>
  <div class="tiles-grid">
    {tiles_html}
  </div>
</div>
<script>
function go(target) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('navigate:' + target);
  }}
}}
document.getElementById('search-form').addEventListener('submit', function(e) {{
  e.preventDefault();
  var val = document.getElementById('search-input').value.trim();
  if (val) {{
    go(val);
  }}
}});
</script>
</body>
</html>"#
    )
}

fn render_tile(title: &str, url: &str, visits: Option<i64>) -> String {
    let host = extract_display_host(url);
    let initial = host.chars().next().unwrap_or('W').to_ascii_uppercase();
    let meta = match visits {
        Some(v) if v > 1 => format!("{} &middot; {} visits", escape_html(&host), v),
        _ => escape_html(&host),
    };
    let escaped_url = escape_js_attr(url);
    format!(
        r#"<div class="tile-card" onclick="go('{escaped_url}')">
  <div class="tile-icon">{initial}</div>
  <div class="tile-title">{title}</div>
  <div class="tile-meta">{meta}</div>
</div>"#,
        title = escape_html(title),
    )
}

fn render_history_html(entries: &[HistoryEntry]) -> String {
    let mut rows = String::new();
    if entries.is_empty() {
        rows.push_str(
            r#"<div class="empty-state">No browsing history recorded yet. Pages you visit will appear here.</div>"#,
        );
    } else {
        for item in entries {
            let escaped_url = escape_js_attr(&item.url);
            let host = extract_display_host(&item.url);
            let initial = host.chars().next().unwrap_or('W').to_ascii_uppercase();
            rows.push_str(&format!(
                r#"<div class="list-item" data-search="{search}">
  <div class="item-main" onclick="go('{escaped_url}')">
    <div class="item-badge">{initial}</div>
    <div class="item-text">
      <div class="item-title">{title}</div>
      <div class="item-url">{url}</div>
    </div>
  </div>
  <div class="item-actions">
    <span class="visit-pill">{visits} visit{plural}</span>
    <button class="icon-del" title="Remove from history" onclick="delHistory('{escaped_url}')">&times;</button>
  </div>
</div>"#,
                search = escape_html(&format!(
                    "{} {}",
                    item.title.to_ascii_lowercase(),
                    item.url.to_ascii_lowercase()
                )),
                title = escape_html(&item.title),
                url = escape_html(&item.url),
                visits = item.visit_count,
                plural = if item.visit_count == 1 { "" } else { "s" },
            ));
        }
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>History</title>
<style>
{SHARED_INTERNAL_CSS}
.page-wrap {{ max-width: 820px; margin: 0 auto; padding: 36px 24px 64px; }}
.page-header {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
}}
.page-header h1 {{ font-size: 24px; font-weight: 700; }}
.toolbar-row {{
  display: flex;
  gap: 12px;
  margin-bottom: 20px;
}}
.filter-input {{
  flex: 1;
  height: 38px;
  padding: 0 14px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  font-size: 14px;
  outline: none;
}}
.filter-input:focus {{ border-color: var(--accent); }}
.list-card {{
  border: 1px solid var(--border);
  border-radius: 14px;
  background: var(--surface);
  overflow: hidden;
}}
.list-item {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid var(--border);
  gap: 12px;
}}
.list-item:last-child {{ border-bottom: none; }}
.list-item:hover {{ background: var(--surface-hover); }}
.item-main {{
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
  cursor: pointer;
}}
.item-badge {{
  width: 32px;
  height: 32px;
  border-radius: 8px;
  background: var(--surface-hover);
  color: var(--accent);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: 14px;
  flex-shrink: 0;
}}
.item-text {{ min-width: 0; flex: 1; }}
.item-title {{
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.item-url {{
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.item-actions {{ display: flex; align-items: center; gap: 10px; flex-shrink: 0; }}
.visit-pill {{
  font-size: 11.5px;
  color: var(--muted);
  padding: 3px 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
}}
.icon-del {{
  border: none;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  padding: 4px 8px;
  border-radius: 6px;
}}
.icon-del:hover {{ color: var(--danger); background: var(--surface-hover); }}
.empty-state {{ padding: 42px 24px; text-align: center; color: var(--muted); font-size: 14px; }}
</style>
</head>
<body>
<div class="page-wrap">
  <div class="page-header">
    <h1>Browsing History</h1>
    <div class="nav-links">
      <button class="pill-btn" onclick="go('ember://newtab')">New Tab</button>
      <button class="pill-btn" onclick="go('ember://bookmarks')">&#9733; Bookmarks</button>
      <button class="pill-btn danger" onclick="clearHistory()">Clear History</button>
    </div>
  </div>
  <div class="toolbar-row">
    <input class="filter-input" id="filter-input" type="text" placeholder="Filter history by title or URL..." oninput="filterList(this.value)" />
  </div>
  <div class="list-card" id="list-card">
    {rows}
  </div>
</div>
<script>
function go(target) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('navigate:' + target);
  }}
}}
function clearHistory() {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('history_clear:');
  }}
}}
function delHistory(url) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('history_delete:' + url);
  }}
}}
function filterList(q) {{
  var needle = (q || '').toLowerCase().trim();
  var items = document.querySelectorAll('.list-item');
  for (var i = 0; i < items.length; i++) {{
    var hay = items[i].getAttribute('data-search') || '';
    items[i].style.display = (!needle || hay.indexOf(needle) !== -1) ? 'flex' : 'none';
  }}
}}
</script>
</body>
</html>"#
    )
}

fn render_bookmarks_html(entries: &[BookmarkEntry]) -> String {
    let mut rows = String::new();
    if entries.is_empty() {
        rows.push_str(
            r#"<div class="empty-state">No bookmarks saved yet. Press <b>Ctrl/Cmd+D</b> or click the &#9733; button in the address bar to bookmark any page.</div>"#,
        );
    } else {
        for item in entries {
            let escaped_url = escape_js_attr(&item.url);
            let host = extract_display_host(&item.url);
            let initial = host.chars().next().unwrap_or('B').to_ascii_uppercase();
            rows.push_str(&format!(
                r#"<div class="list-item" data-search="{search}">
  <div class="item-main" onclick="go('{escaped_url}')">
    <div class="item-badge">{initial}</div>
    <div class="item-text">
      <div class="item-title">{title}</div>
      <div class="item-url">{url}</div>
    </div>
  </div>
  <div class="item-actions">
    <button class="icon-del" title="Remove bookmark" onclick="removeBookmark('{escaped_url}')">Remove</button>
  </div>
</div>"#,
                search = escape_html(&format!(
                    "{} {}",
                    item.title.to_ascii_lowercase(),
                    item.url.to_ascii_lowercase()
                )),
                title = escape_html(&item.title),
                url = escape_html(&item.url),
            ));
        }
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Bookmarks</title>
<style>
{SHARED_INTERNAL_CSS}
.page-wrap {{ max-width: 820px; margin: 0 auto; padding: 36px 24px 64px; }}
.page-header {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
}}
.page-header h1 {{ font-size: 24px; font-weight: 700; }}
.toolbar-row {{
  display: flex;
  gap: 12px;
  margin-bottom: 20px;
}}
.filter-input {{
  flex: 1;
  height: 38px;
  padding: 0 14px;
  border-radius: 10px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
  font-size: 14px;
  outline: none;
}}
.filter-input:focus {{ border-color: var(--accent); }}
.list-card {{
  border: 1px solid var(--border);
  border-radius: 14px;
  background: var(--surface);
  overflow: hidden;
}}
.list-item {{
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 16px;
  border-bottom: 1px solid var(--border);
  gap: 12px;
}}
.list-item:last-child {{ border-bottom: none; }}
.list-item:hover {{ background: var(--surface-hover); }}
.item-main {{
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
  cursor: pointer;
}}
.item-badge {{
  width: 32px;
  height: 32px;
  border-radius: 8px;
  background: var(--surface-hover);
  color: var(--accent);
  display: flex;
  align-items: center;
  justify-content: center;
  font-weight: 700;
  font-size: 14px;
  flex-shrink: 0;
}}
.item-text {{ min-width: 0; flex: 1; }}
.item-title {{
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.item-url {{
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}}
.item-actions {{ display: flex; align-items: center; gap: 10px; flex-shrink: 0; }}
.icon-del {{
  border: 1px solid var(--border);
  background: transparent;
  color: var(--muted);
  cursor: pointer;
  padding: 5px 10px;
  border-radius: 8px;
  font-size: 12px;
}}
.icon-del:hover {{ color: var(--danger); border-color: var(--danger); }}
.empty-state {{ padding: 42px 24px; text-align: center; color: var(--muted); font-size: 14px; }}
</style>
</head>
<body>
<div class="page-wrap">
  <div class="page-header">
    <h1>Bookmarks</h1>
    <div class="nav-links">
      <button class="pill-btn" onclick="go('ember://newtab')">New Tab</button>
      <button class="pill-btn" onclick="go('ember://history')">History</button>
    </div>
  </div>
  <div class="toolbar-row">
    <input class="filter-input" id="filter-input" type="text" placeholder="Search bookmarks..." oninput="filterList(this.value)" />
  </div>
  <div class="list-card">
    {rows}
  </div>
</div>
<script>
function go(target) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('navigate:' + target);
  }}
}}
function removeBookmark(url) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('bookmark_remove:' + url);
  }}
}}
function filterList(q) {{
  var needle = (q || '').toLowerCase().trim();
  var items = document.querySelectorAll('.list-item');
  for (var i = 0; i < items.length; i++) {{
    var hay = items[i].getAttribute('data-search') || '';
    items[i].style.display = (!needle || hay.indexOf(needle) !== -1) ? 'flex' : 'none';
  }}
}}
</script>
</body>
</html>"#
    )
}

fn render_downloads_html(downloads_dir: &std::path::Path) -> String {
    let dir_escaped = escape_html(&downloads_dir.to_string_lossy());
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Downloads</title>
<style>{SHARED_INTERNAL_CSS}
.page-wrap {{ max-width: 820px; margin: 0 auto; padding: 36px 24px 64px; }}
</style>
</head>
<body>
<div class="page-wrap">
  <div class="header">
    <div>
      <h1>Downloads</h1>
      <div class="sub">Files are saved to <code>{dir_escaped}</code></div>
    </div>
    <div class="nav-pills">
      <button class="pill-btn" onclick="go('ember://newtab')">New Tab</button>
      <button class="pill-btn" onclick="go('ember://history')">History</button>
      <button class="pill-btn" onclick="go('ember://bookmarks')">&#9733; Bookmarks</button>
      <button class="pill-btn" onclick="go('ember://settings')">&#9881; Settings</button>
    </div>
  </div>
  <div class="list-card">
    <div class="empty">Downloaded files are stored in <code>{dir_escaped}</code>.</div>
  </div>
</div>
<script>
function go(target) {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('navigate:' + target);
  }}
}}
</script>
</body>
</html>"#
    )
}

fn extract_display_host(url: &str) -> String {
    let after_scheme = url.find("://").map(|idx| &url[idx + 3..]).unwrap_or(url);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_js_attr(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("&quot;"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' | '\n' => {}
            _ => out.push(ch),
        }
    }
    out
}

const SETTINGS_CSS: &str = r#"
.page-wrap { max-width: 760px; margin: 0 auto; padding: 36px 24px 80px; }
.page-header { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 24px; }
.page-header h1 { font-size: 24px; font-weight: 700; }
.section { margin-bottom: 28px; }
.section h2 { font-size: 12px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 0 4px 10px; }
.card { border: 1px solid var(--border); border-radius: 14px; background: var(--surface); overflow: hidden; }
.row { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px 18px; border-bottom: 1px solid var(--border); }
.row:last-child { border-bottom: none; }
.row-text { min-width: 0; flex: 1; }
.row-title { font-size: 14px; font-weight: 600; }
.row-desc { font-size: 12px; color: var(--muted); margin-top: 2px; line-height: 1.4; }
.ctl { height: 34px; padding: 0 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); font-size: 13px; outline: none; min-width: 190px; }
.ctl:focus { border-color: var(--accent); }
.switch { appearance: none; -webkit-appearance: none; width: 38px; height: 22px; border-radius: 11px; background: var(--border); position: relative; cursor: pointer; flex-shrink: 0; transition: background 0.15s; }
.switch::after { content: ""; position: absolute; top: 3px; left: 3px; width: 16px; height: 16px; border-radius: 50%; background: #fff; transition: transform 0.15s; }
.switch:checked { background: var(--accent); }
.switch:checked::after { transform: translateX(16px); }
.btn { padding: 7px 14px; border-radius: 8px; border: 1px solid var(--border); background: var(--surface); color: var(--text); font-size: 13px; font-weight: 500; cursor: pointer; }
.btn:hover:not(:disabled) { background: var(--surface-hover); }
.btn:disabled { opacity: 0.5; cursor: default; }
.btn.primary { background: var(--accent); border-color: var(--accent); color: #fff; }
.btn.danger { color: var(--danger); border-color: var(--danger); }
.checks { display: flex; gap: 18px; flex-wrap: wrap; padding: 14px 18px 4px; }
.checks label { display: flex; align-items: center; gap: 7px; font-size: 13px; cursor: pointer; }
.actions { display: flex; align-items: center; gap: 12px; padding: 10px 18px 16px; }
.status { font-size: 12px; color: var(--muted); }
.allow-item { display: flex; align-items: center; justify-content: space-between; padding: 9px 18px; border-bottom: 1px solid var(--border); font-size: 13px; }
.allow-item:last-child { border-bottom: none; }
.allow-empty { padding: 14px 18px; font-size: 13px; color: var(--muted); }
.allow-add { display: flex; gap: 10px; padding: 12px 18px; border-top: 1px solid var(--border); }
.allow-add .ctl { flex: 1; }
#saved { position: fixed; bottom: 22px; right: 24px; padding: 8px 14px; border-radius: 8px; background: var(--text); color: var(--bg); font-size: 12px; opacity: 0; transition: opacity 0.2s; pointer-events: none; }
#saved.show { opacity: 0.92; }
"#;

const SETTINGS_JS: &str = r#"
function send(m) { if (window.ipc && typeof window.ipc.postMessage === 'function') { window.ipc.postMessage(m); } }
function flash() {
  var el = document.getElementById('saved');
  el.classList.add('show');
  clearTimeout(window.__savedTimer);
  window.__savedTimer = setTimeout(function () { el.classList.remove('show'); }, 1200);
}
function setVal(key, value) { send('settings_set:' + key + '=' + value); flash(); }
function setBool(key, checked) { setVal(key, checked ? 'true' : 'false'); }
function act(name, arg) { send('settings_action:' + name + (arg ? ':' + arg : '')); }
function clearData() {
  var parts = [];
  ['history', 'cookies', 'cache'].forEach(function (id) {
    if (document.getElementById('cd-' + id).checked) { parts.push(id); }
  });
  var status = document.getElementById('clear-status');
  if (!parts.length) { status.textContent = 'Select at least one item to clear.'; return; }
  act('clear_data', parts.join(','));
  status.textContent = 'Cleared: ' + parts.join(', ');
}
function addAllow() {
  var input = document.getElementById('allow-input');
  var v = input.value.trim();
  if (v) { act('allowlist_add', v); }
}
document.addEventListener('keydown', function (e) {
  if (e.key === 'Enter' && document.activeElement && document.activeElement.id === 'allow-input') { addAllow(); }
});
"#;

fn switch_row(key: &str, title: &str, desc: &str, checked: bool) -> String {
    format!(
        r#"<div class="row"><div class="row-text"><div class="row-title">{title}</div><div class="row-desc">{desc}</div></div><input type="checkbox" class="switch" {checked} onchange="setBool('{key}', this.checked)"></div>"#,
        checked = if checked { "checked" } else { "" },
    )
}

fn select_row(
    key: &str,
    title: &str,
    desc: &str,
    options: &[(String, String)],
    current: &str,
) -> String {
    let mut opts = String::new();
    for (value, label) in options {
        opts.push_str(&format!(
            r#"<option value="{v}"{sel}>{l}</option>"#,
            v = escape_html(value),
            l = escape_html(label),
            sel = if value == current { " selected" } else { "" },
        ));
    }
    format!(
        r#"<div class="row"><div class="row-text"><div class="row-title">{title}</div><div class="row-desc">{desc}</div></div><select class="ctl" onchange="setVal('{key}', this.value)">{opts}</select></div>"#
    )
}

fn format_last_updated(epoch: Option<u64>) -> String {
    let Some(then) = epoch else {
        return String::from("Never (using built-in starter rules)");
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(then);
    let age = now.saturating_sub(then);
    if age < 60 {
        String::from("just now")
    } else if age < 3600 {
        format!("{} min ago", age / 60)
    } else if age < 86_400 {
        format!("{} h ago", age / 3600)
    } else {
        format!("{} d ago", age / 86_400)
    }
}

fn render_settings_html(
    settings: &BrowserSettings,
    config_path: &std::path::Path,
    adblocker: &Adblocker,
) -> String {
    let own = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
            .collect()
    };

    let engines = own(&[
        ("duckduckgo", "DuckDuckGo"),
        ("google", "Google"),
        ("brave", "Brave Search"),
        ("bing", "Bing"),
        ("startpage", "Startpage"),
    ]);
    let themes = own(&[
        ("system", "Use system setting"),
        ("light", "Light"),
        ("dark", "Dark"),
    ]);
    let mut suspend = own(&[
        ("0", "Never"),
        ("1", "After 1 minute"),
        ("5", "After 5 minutes"),
        ("10", "After 10 minutes"),
        ("15", "After 15 minutes"),
        ("30", "After 30 minutes"),
        ("60", "After 1 hour"),
    ]);
    let current_suspend = settings.tab_suspend_minutes.to_string();
    if !suspend.iter().any(|(v, _)| *v == current_suspend) {
        suspend.push((
            current_suspend.clone(),
            format!("After {current_suspend} minutes"),
        ));
    }

    let mut general = String::new();
    general.push_str(&select_row(
        "search_engine",
        "Default search engine",
        "Used for searches typed in the address bar.",
        &engines,
        settings.search_engine.as_str(),
    ));
    general.push_str(&format!(
        r#"<div class="row"><div class="row-text"><div class="row-title">Homepage</div><div class="row-desc">Opened by the Home button and at startup. Use <code>ember://newtab</code> for the new tab page.</div></div><input class="ctl" type="text" value="{}" spellcheck="false" onchange="setVal('homepage', this.value)"></div>"#,
        escape_html(&settings.homepage)
    ));
    general.push_str(&select_row(
        "theme",
        "Theme",
        "Applies to the window frame, toolbar and internal pages.",
        &themes,
        settings.theme.as_str(),
    ));
    general.push_str(&select_row(
        "tab_suspend_minutes",
        "Suspend background tabs",
        "Frees memory by unloading tabs you have not used for a while. They reload when selected.",
        &suspend,
        &current_suspend,
    ));
    general.push_str(&switch_row(
        "tab_animations_enabled",
        "Enable tab animations",
        "Animates tab opening, closing, and drag reordering. Turn off for instant layout without transitions.",
        settings.tab_animations_enabled,
    ));

    let updating = adblocker.is_updating();
    let status_text = if updating {
        String::from("Updating filter lists...")
    } else {
        format!(
            "Includes uBlock Origin filters (Unbreak, Annoyances, Quick fixes every 6h; others every 24h). Last updated: {} | Quick fixes: {}",
            format_last_updated(adblocker.last_updated_epoch()),
            format_last_updated(adblocker.quick_fixes_last_updated_epoch())
        )
    };

    let mut ads = String::new();
    ads.push_str(&switch_row(
        "adblock_enabled",
        "Block ads and trackers",
        "Master switch for the built-in adblocker.",
        settings.adblock_enabled,
    ));
    ads.push_str(&switch_row(
        "easylist_enabled",
        "EasyList",
        "General ad blocking rules.",
        settings.easylist_enabled,
    ));
    ads.push_str(&switch_row(
        "easyprivacy_enabled",
        "EasyPrivacy",
        "Blocks tracking scripts and analytics beacons.",
        settings.easyprivacy_enabled,
    ));
    ads.push_str(&format!(
        r#"<div class="row"><div class="row-text"><div class="row-title">Filter lists</div><div class="row-desc">{status}</div></div><button class="btn primary" {dis} onclick="act('update_filters')">Update filters now</button></div>"#,
        status = escape_html(&status_text),
        dis = if updating { "disabled" } else { "" },
    ));

    let mut allow = String::new();
    let hosts = adblocker.allowlist_hosts();
    if hosts.is_empty() {
        allow.push_str(r#"<div class="allow-empty">No sites are allowlisted. Use the shield button in the toolbar to pause blocking on a site.</div>"#);
    } else {
        for host in &hosts {
            allow.push_str(&format!(
                r#"<div class="allow-item"><span>{h}</span><button class="btn" onclick="act('allowlist_remove', '{j}')">Remove</button></div>"#,
                h = escape_html(host),
                j = escape_js_attr(host),
            ));
        }
    }
    allow.push_str(
        r#"<div class="allow-add"><input id="allow-input" class="ctl" type="text" placeholder="example.com" spellcheck="false"><button class="btn" onclick="addAllow()">Add site</button></div>"#,
    );

    let mut privacy = String::new();
    privacy.push_str(&switch_row(
        "block_third_party_cookies",
        "Block third-party cookies",
        "Strips cookies from requests that a page makes to other sites. Applies immediately.",
        settings.block_third_party_cookies,
    ));
    privacy.push_str(&switch_row(
        "send_dnt_and_gpc",
        "Send Do Not Track and Global Privacy Control",
        "Adds the <code>DNT: 1</code> and <code>Sec-GPC: 1</code> headers to requests. Page scripts see the signal in newly opened tabs.",
        settings.send_dnt_and_gpc,
    ));
    privacy.push_str(&switch_row(
        "https_only_mode",
        "HTTPS-only mode",
        "Shows a warning before loading sites that only offer insecure HTTP.",
        settings.https_only_mode,
    ));
    privacy.push_str(&switch_row(
        "search_suggestions_enabled",
        "Show online search suggestions",
        "Fetches autocomplete suggestions from your default search engine as you type. Note: typed text in the address bar is sent to the search engine.",
        settings.search_suggestions_enabled,
    ));

    let clear = r#"<div class="checks"><label><input type="checkbox" id="cd-history" checked> Browsing history</label><label><input type="checkbox" id="cd-cookies"> Cookies and site data</label><label><input type="checkbox" id="cd-cache"> Cached files</label></div><div class="actions"><button class="btn danger" onclick="clearData()">Clear data</button><span id="clear-status" class="status"></span></div>"#;

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Settings</title>
<style>{SHARED_INTERNAL_CSS}{SETTINGS_CSS}</style>
</head>
<body>
<div class="page-wrap">
  <div class="page-header">
    <h1>Settings</h1>
    <div class="nav-links">
      <button class="pill-btn" onclick="act('open', 'ember://newtab')">New Tab</button>
      <button class="pill-btn" onclick="act('open', 'ember://history')">History</button>
      <button class="pill-btn" onclick="act('open', 'ember://bookmarks')">Bookmarks</button>
    </div>
  </div>
  <div class="section"><h2>General</h2><div class="card">{general}</div></div>
  <div class="section"><h2>Ad blocker</h2><div class="card">{ads}</div></div>
  <div class="section"><h2>Allowed sites</h2><div class="card">{allow}</div></div>
  <div class="section"><h2>Privacy and security</h2><div class="card">{privacy}</div></div>
  <div class="section"><h2>Clear browsing data</h2><div class="card">{clear}</div></div>
  <div class="status" style="text-align:center;padding:12px 0 24px;">Configuration stored in <code>{config_path}</code></div>
</div>
<div id="saved">Saved</div>
<script>{SETTINGS_JS}</script>
</body>
</html>"#,
        config_path = escape_html(&config_path.display().to_string()),
    )
}

fn render_adblock_debug_html(adblocker: &Adblocker) -> String {
    let (tab_id, snapshot) = adblocker.latest_tab_debug_snapshot();
    let updating = adblocker.is_updating();
    let page_url = if snapshot.page_url.is_empty() {
        String::from("(no web tab loaded yet)")
    } else {
        snapshot.page_url.clone()
    };

    let mut rows_html = String::new();
    for entry in snapshot.entries.iter().rev() {
        rows_html.push_str(&format!(
            r#"<tr><td><span class="badge badge-{cls}">{kind}</span></td><td class="mono">{rule}</td><td>{rtype}</td><td class="mono url-col">{target}</td></tr>"#,
            cls = escape_html(entry.kind.label()),
            kind = escape_html(entry.kind.label()),
            rule = escape_html(&entry.rule_or_name),
            rtype = escape_html(&entry.request_type),
            target = escape_html(&entry.target_url),
        ));
    }
    if rows_html.is_empty() {
        rows_html.push_str(r#"<tr><td colspan="4" class="empty">No matched rules or scriptlets recorded for this tab yet.</td></tr>"#);
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Adblock Debug</title>
<style>
{SHARED_INTERNAL_CSS}{SETTINGS_CSS}
.dbg-wrap {{ max-width: 1080px; margin: 0 auto; padding: 36px 24px; }}
.dbg-head {{ display: flex; justify-content: space-between; align-items: center; gap: 16px; margin-bottom: 18px; flex-wrap: wrap; }}
.dbg-meta {{ color: var(--muted); font-size: 13px; margin-bottom: 18px; line-height: 1.6; }}
.dbg-table {{ width: 100%; border-collapse: collapse; background: var(--surface); border: 1px solid var(--border); border-radius: 12px; overflow: hidden; font-size: 12.5px; }}
.dbg-table th, .dbg-table td {{ text-align: left; padding: 9px 12px; border-bottom: 1px solid var(--border); vertical-align: top; }}
.dbg-table th {{ background: var(--surface-2); font-weight: 600; color: var(--muted); }}
.mono {{ font-family: ui-monospace, SFMono-Regular, Consolas, monospace; word-break: break-all; }}
.url-col {{ max-width: 380px; color: var(--muted); }}
.badge {{ display: inline-block; padding: 2px 8px; border-radius: 999px; font-size: 11px; font-weight: 600; white-space: nowrap; }}
.badge-scriptlet {{ background: rgba(16, 185, 129, 0.16); color: #10b981; }}
.badge-network-block {{ background: rgba(239, 68, 68, 0.16); color: #ef4444; }}
.badge-redirect {{ background: rgba(245, 158, 11, 0.16); color: #f59e0b; }}
.badge-cosmetic {{ background: rgba(59, 130, 246, 0.16); color: #3b82f6; }}
.empty {{ text-align: center; color: var(--muted); padding: 28px 12px; }}
</style>
</head>
<body>
<div class="dbg-wrap">
  <div class="dbg-head">
    <div>
      <h1>Adblock Debug</h1>
      <div class="dbg-meta">
        Tab #{tab_id}: <span class="mono">{page_url}</span><br>
        Standard lists updated: {std_updated} &middot; Quick fixes (6h): {qf_updated} &middot; Matches shown: {count}
      </div>
    </div>
    <div style="display:flex;gap:8px;">
      <button class="btn" onclick="location.reload()">Refresh</button>
      <button class="btn primary" {dis} onclick="updateNow()">Update filters now</button>
    </div>
  </div>
  <table class="dbg-table">
    <thead><tr><th>Kind</th><th>Rule / Scriptlet</th><th>Type</th><th>Target URL</th></tr></thead>
    <tbody>{rows_html}</tbody>
  </table>
</div>
<script>
function updateNow() {{
  if (window.ipc && typeof window.ipc.postMessage === 'function') {{
    window.ipc.postMessage('settings_action:update_filters');
    setTimeout(function() {{ location.reload(); }}, 1500);
  }}
}}
</script>
</body>
</html>"#,
        tab_id = tab_id,
        page_url = escape_html(&page_url),
        std_updated = escape_html(&format_last_updated(adblocker.last_updated_epoch())),
        qf_updated = escape_html(&format_last_updated(
            adblocker.quick_fixes_last_updated_epoch()
        )),
        count = snapshot.entries.len(),
        dis = if updating { "disabled" } else { "" },
    )
}

fn render_https_warning_html(target: &str) -> String {
    let host = extract_display_host(target);
    let https_alt = target
        .strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_default();
    let https_button = if https_alt.is_empty() {
        String::new()
    } else {
        format!(
            r#"<button class="btn" onclick="go('{}')">Try the HTTPS version</button>"#,
            escape_js_attr(&https_alt)
        )
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Connection not secure</title>
<style>
{SHARED_INTERNAL_CSS}{SETTINGS_CSS}
.warn-wrap {{ max-width: 560px; margin: 0 auto; padding: 96px 24px; }}
.warn-icon {{ width: 56px; height: 56px; border-radius: 16px; background: rgba(220, 38, 38, 0.12); color: var(--danger); display: flex; align-items: center; justify-content: center; font-size: 30px; font-weight: 700; margin-bottom: 22px; }}
.warn-wrap h1 {{ font-size: 26px; margin-bottom: 12px; }}
.warn-wrap p {{ color: var(--muted); font-size: 14px; line-height: 1.6; margin-bottom: 10px; }}
.warn-host {{ font-weight: 600; color: var(--text); word-break: break-all; }}
.warn-actions {{ display: flex; gap: 10px; flex-wrap: wrap; margin-top: 26px; }}
.btn.unsafe {{ background: transparent; border-color: var(--border); color: var(--muted); }}
</style>
</head>
<body>
<div class="warn-wrap">
  <div class="warn-icon">!</div>
  <h1>Connection not secure</h1>
  <p><span class="warn-host">{host}</span> only supports insecure HTTP, and HTTPS-only mode is turned on.</p>
  <p>Anything you send or receive on this site, including passwords and form data, could be read or changed by others on the network.</p>
  <div class="warn-actions">
    <button class="btn primary" onclick="go('ember://newtab')">Go back to safety</button>
    {https_button}
    <button class="btn unsafe" onclick="proceed()">Continue to site anyway</button>
  </div>
</div>
<script>
var TARGET = '{target_js}';
function send(m) {{ if (window.ipc && typeof window.ipc.postMessage === 'function') {{ window.ipc.postMessage(m); }} }}
function go(t) {{ send('navigate:' + t); }}
function proceed() {{ send('https_proceed:' + TARGET); }}
</script>
</body>
</html>"#,
        host = escape_html(&host),
        target_js = escape_js_script_string(target),
    )
}

/// Escapes `input` for use inside a single-quoted JavaScript string within a `<script>` element.
fn escape_js_script_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\x22"),
            '<' => out.push_str("\\x3C"),
            '>' => out.push_str("\\x3E"),
            '&' => out.push_str("\\x26"),
            '\r' | '\n' => {}
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_internal_url_routing() {
        assert!(is_internal_url("ember://newtab"));
        assert!(is_internal_url("EMBER://history"));
        assert!(!is_internal_url("https://example.com"));
        assert_eq!(canonical_internal_url("ember://history"), Some(HISTORY_URL));
        assert_eq!(
            canonical_internal_url("ember://bookmarks/"),
            Some(BOOKMARKS_URL)
        );
        assert_eq!(canonical_internal_url("ember://newtab"), Some(NEWTAB_URL));
        assert_eq!(
            canonical_internal_url("ember://settings"),
            Some(SETTINGS_URL)
        );
        assert_eq!(
            canonical_internal_url("ember://warning?url=http%3A%2F%2Fa.com"),
            Some(WARNING_URL)
        );
    }

    #[test]
    fn test_warning_url_roundtrip() {
        let target = "http://example.com/path?a=1&b=two words#frag";
        let url = warning_url_for(target);
        assert!(url.starts_with("ember://warning?url="));
        assert_eq!(canonical_internal_url(&url), Some(WARNING_URL));
        assert_eq!(
            warning_target(&url).as_deref(),
            Some("http://example.com/path?a=1&b=two words#frag")
        );
    }

    #[test]
    fn test_settings_page_escapes_and_reflects_values() {
        let settings = BrowserSettings {
            homepage: String::from("https://x.test/?q=\"><script>alert(1)</script>"),
            ..Default::default()
        };
        let html = render_settings_html(
            &settings,
            std::path::Path::new("/tmp/settings.toml"),
            &Adblocker::new(),
        );
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&quot;&gt;&lt;script&gt;"));
        assert!(html.contains(r#"<option value="duckduckgo" selected>"#));
    }

    #[test]
    fn test_warning_page_escapes_target() {
        let html = render_https_warning_html("http://evil.test/'</script><script>alert(1)//");
        assert!(!html.contains("</script><script>alert(1)"));
        assert!(html.contains("evil.test"));
    }
}
