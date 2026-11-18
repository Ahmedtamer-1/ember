use std::path::{Path, PathBuf};

/// Maximum length (in characters) of a sanitized download file name, including the extension.
const MAX_FILENAME_CHARS: usize = 200;

/// Extensions longer than this are treated as part of the stem when truncating.
const MAX_PRESERVED_EXTENSION_CHARS: usize = 16;

/// Status of a file download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    InProgress,
    Completed,
    Failed,
}

impl DownloadStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Metadata for a single tracked download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    pub id: usize,
    pub url: String,
    pub file_name: String,
    pub save_path: PathBuf,
    pub status: DownloadStatus,
}

/// Manages active and completed downloads for the browser session.
#[derive(Debug, Default)]
pub struct DownloadManager {
    items: Vec<DownloadItem>,
    next_id: usize,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }

    /// Resolves a destination file path inside `downloads_dir` and records a new `InProgress` download.
    ///
    /// The destination is unique both on disk and among downloads that are still in progress
    /// (whose files may not exist yet), so two simultaneous downloads of `file.zip` get
    /// `file.zip` and `file (1).zip`.
    pub fn register_download_started(
        &mut self,
        url: &str,
        suggested_path: &Path,
        downloads_dir: &Path,
    ) -> PathBuf {
        let file_name = suggested_path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
            .unwrap_or_else(|| extract_filename_from_url(url));

        let reserved: Vec<&Path> = self
            .items
            .iter()
            .filter(|d| d.status == DownloadStatus::InProgress)
            .map(|d| d.save_path.as_path())
            .collect();
        let dest_path = unique_destination_path(downloads_dir, &file_name, &reserved);
        let final_name = dest_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&file_name)
            .to_string();

        let id = self.next_id;
        self.next_id += 1;

        self.items.insert(
            0,
            DownloadItem {
                id,
                url: url.to_string(),
                file_name: final_name,
                save_path: dest_path.clone(),
                status: DownloadStatus::InProgress,
            },
        );
        dest_path
    }

    /// Updates the status of the in-progress download that just finished.
    ///
    /// Matching order: the completed file `path` (most reliable, survives redirects), then the
    /// download `url` (oldest in-progress first), and finally, only when exactly one download is
    /// in progress, that single download. Otherwise the event is ignored rather than marking an
    /// unrelated download.
    pub fn mark_download_completed(&mut self, url: &str, path: Option<PathBuf>, success: bool) {
        let new_status = if success {
            DownloadStatus::Completed
        } else {
            DownloadStatus::Failed
        };

        let in_progress = |d: &DownloadItem| d.status == DownloadStatus::InProgress;

        let by_path = path.as_deref().and_then(|p| {
            self.items
                .iter()
                .position(|d| in_progress(d) && paths_equal(&d.save_path, p))
        });
        // `items` is newest-first; the oldest matching download most likely finished first.
        let by_url = || {
            self.items
                .iter()
                .rposition(|d| in_progress(d) && d.url == url)
        };
        let single = || {
            let mut it = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, d)| d.status == DownloadStatus::InProgress);
            match (it.next(), it.next()) {
                (Some((idx, _)), None) => Some(idx),
                _ => None,
            }
        };

        let Some(idx) = by_path.or_else(by_url).or_else(single) else {
            return;
        };
        let Some(item) = self.items.get_mut(idx) else {
            return;
        };
        item.status = new_status;
        if let Some(p) = path {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                item.file_name = name.to_string();
            }
            item.save_path = p;
        }
    }

    /// Clears completed and failed downloads from the list.
    pub fn clear_finished(&mut self) {
        self.items
            .retain(|d| d.status == DownloadStatus::InProgress);
    }

    /// Returns all tracked downloads (newest first).
    pub fn items(&self) -> &[DownloadItem] {
        &self.items
    }
}

/// Compares paths the way the host filesystem does (case-insensitively on Windows/macOS).
fn paths_equal(a: &Path, b: &Path) -> bool {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn extract_filename_from_url(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last_segment = without_query.rsplit('/').next().unwrap_or("").trim();
    if last_segment.is_empty() || last_segment.contains(':') {
        String::from("download.bin")
    } else {
        sanitize_filename(last_segment)
    }
}

/// Returns `true` if `name` is a Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`,
/// `COM1`-`COM9`, `LPT1`-`LPT9`), case-insensitive, with or without an extension.
fn is_windows_reserved_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = base.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            let bytes = upper.as_bytes();
            bytes.len() == 4
                && (upper.starts_with("COM") || upper.starts_with("LPT"))
                && (b'1'..=b'9').contains(&bytes[3])
        }
    }
}

fn trim_trailing_dots_and_spaces(s: &str) -> &str {
    s.trim_end_matches(['.', ' '])
}

/// Truncates `name` to at most `MAX_FILENAME_CHARS` characters, keeping a short extension.
fn truncate_filename(name: &str) -> String {
    if name.chars().count() <= MAX_FILENAME_CHARS {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => {
            let ext = &name[dot..];
            if ext.chars().count() <= MAX_PRESERVED_EXTENSION_CHARS + 1 {
                (&name[..dot], ext)
            } else {
                (name, "")
            }
        }
        _ => (name, ""),
    };
    let keep = MAX_FILENAME_CHARS.saturating_sub(ext.chars().count());
    let truncated: String = stem.chars().take(keep).collect();
    let truncated = trim_trailing_dots_and_spaces(&truncated);
    let truncated = if truncated.is_empty() {
        "download"
    } else {
        truncated
    };
    format!("{truncated}{ext}")
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control() {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    // Windows silently strips trailing dots/spaces, which can collide with other names.
    let trimmed = trim_trailing_dots_and_spaces(out.trim().trim_start_matches('.')).trim();
    if trimmed.is_empty() {
        return String::from("download.bin");
    }
    let mut safe = truncate_filename(trimmed);
    if is_windows_reserved_name(&safe) {
        safe.insert(0, '_');
        safe = truncate_filename(&safe);
    }
    safe
}

fn unique_destination_path(downloads_dir: &Path, file_name: &str, reserved: &[&Path]) -> PathBuf {
    let is_taken = |p: &Path| p.exists() || reserved.iter().any(|r| paths_equal(r, p));

    let safe_name = sanitize_filename(file_name);
    let candidate = downloads_dir.join(&safe_name);
    if !is_taken(&candidate) {
        return candidate;
    }

    let path_obj = Path::new(&safe_name);
    let stem = path_obj
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download");
    let ext = path_obj.extension().and_then(|s| s.to_str());

    for idx in 1..1000 {
        let numbered = match ext {
            Some(e) if !e.is_empty() => format!("{stem} ({idx}).{e}"),
            _ => format!("{stem} ({idx})"),
        };
        let next = downloads_dir.join(numbered);
        if !is_taken(&next) {
            return next;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ember_downloads_test_{}_{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn test_download_manager_lifecycle() {
        let mut mgr = DownloadManager::new();
        let dir = std::env::temp_dir();
        let dest = mgr.register_download_started(
            "https://example.com/archive.zip",
            Path::new("C:\\temp\\archive.zip"),
            &dir,
        );
        assert!(dest.ends_with("archive.zip") || dest.to_string_lossy().contains("archive"));
        assert_eq!(mgr.items().len(), 1);
        assert_eq!(mgr.items()[0].status, DownloadStatus::InProgress);

        mgr.mark_download_completed("https://example.com/archive.zip", Some(dest), true);
        assert_eq!(mgr.items()[0].status, DownloadStatus::Completed);

        mgr.clear_finished();
        assert!(mgr.items().is_empty());
    }

    #[test]
    fn test_simultaneous_downloads_get_distinct_paths() {
        let dir = temp_dir("simultaneous");
        let mut mgr = DownloadManager::new();
        let a = mgr.register_download_started(
            "https://a.example.com/report.pdf",
            Path::new("report.pdf"),
            &dir,
        );
        let b = mgr.register_download_started(
            "https://b.example.com/report.pdf",
            Path::new("report.pdf"),
            &dir,
        );
        assert_ne!(a, b);
        assert_eq!(a, dir.join("report.pdf"));
        assert_eq!(b, dir.join("report (1).pdf"));

        // Once the first finishes, its name stays taken only if the file exists on disk.
        mgr.mark_download_completed("https://a.example.com/report.pdf", Some(a), true);
        let c = mgr.register_download_started(
            "https://c.example.com/report.pdf",
            Path::new("report.pdf"),
            &dir,
        );
        assert_ne!(c, b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_completion_matches_by_path_then_url_then_single() {
        let dir = temp_dir("matching");
        let mut mgr = DownloadManager::new();
        let first = mgr.register_download_started(
            "https://one.example.com/a.zip",
            Path::new("a.zip"),
            &dir,
        );
        let second = mgr.register_download_started(
            "https://two.example.com/b.zip",
            Path::new("b.zip"),
            &dir,
        );

        // Redirected URL, but the completed path identifies the second download.
        mgr.mark_download_completed(
            "https://cdn.example.net/redirected",
            Some(second.clone()),
            true,
        );
        let status_of = |mgr: &DownloadManager, p: &Path| {
            mgr.items()
                .iter()
                .find(|d| d.save_path == p)
                .map(|d| d.status)
        };
        assert_eq!(status_of(&mgr, &second), Some(DownloadStatus::Completed));
        assert_eq!(status_of(&mgr, &first), Some(DownloadStatus::InProgress));

        // Add another; with two in progress, an unknown URL/path must not mark anything.
        let third = mgr.register_download_started(
            "https://three.example.com/c.zip",
            Path::new("c.zip"),
            &dir,
        );
        mgr.mark_download_completed("https://unknown.example.com/x", None, false);
        assert_eq!(status_of(&mgr, &first), Some(DownloadStatus::InProgress));
        assert_eq!(status_of(&mgr, &third), Some(DownloadStatus::InProgress));

        // URL match without a path.
        mgr.mark_download_completed("https://three.example.com/c.zip", None, false);
        assert_eq!(status_of(&mgr, &third), Some(DownloadStatus::Failed));

        // Exactly one in progress: fallback applies even for an unknown URL.
        mgr.mark_download_completed("https://unknown.example.com/y", None, true);
        assert_eq!(status_of(&mgr, &first), Some(DownloadStatus::Completed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sanitize_reserved_names_and_trailing_dots() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("Nul.tar.gz"), "_Nul.tar.gz");
        assert_eq!(sanitize_filename("com1.log"), "_com1.log");
        assert_eq!(sanitize_filename("LPT9"), "_LPT9");
        assert_eq!(sanitize_filename("aux "), "_aux");
        // Not reserved
        assert_eq!(sanitize_filename("COM0.txt"), "COM0.txt");
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("LPT10"), "LPT10");
        // Trailing dots/spaces stripped
        assert_eq!(sanitize_filename("report. . "), "report");
        assert_eq!(sanitize_filename("file.txt..."), "file.txt");
        assert_eq!(sanitize_filename("..."), "download.bin");
        assert_eq!(sanitize_filename("a<b>c?.txt"), "a_b_c_.txt");
    }

    #[test]
    fn test_sanitize_caps_length_and_keeps_extension() {
        let long = format!("{}.pdf", "x".repeat(500));
        let safe = sanitize_filename(&long);
        assert_eq!(safe.chars().count(), MAX_FILENAME_CHARS);
        assert!(safe.ends_with(".pdf"));

        let long_unicode = format!("{}.zip", "é".repeat(300));
        let safe = sanitize_filename(&long_unicode);
        assert_eq!(safe.chars().count(), MAX_FILENAME_CHARS);
        assert!(safe.ends_with(".zip"));

        let no_ext = "y".repeat(300);
        assert_eq!(
            sanitize_filename(&no_ext).chars().count(),
            MAX_FILENAME_CHARS
        );

        let short = "short.txt";
        assert_eq!(sanitize_filename(short), short);
    }
}
