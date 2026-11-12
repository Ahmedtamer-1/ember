use std::path::{Path, PathBuf};

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

        let dest_path = unique_destination_path(downloads_dir, &file_name);
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

    /// Updates the status of the most recent matching download upon completion.
    pub fn mark_download_completed(&mut self, url: &str, path: Option<PathBuf>, success: bool) {
        let new_status = if success {
            DownloadStatus::Completed
        } else {
            DownloadStatus::Failed
        };

        if let Some(item) = self
            .items
            .iter_mut()
            .find(|d| d.status == DownloadStatus::InProgress && d.url == url)
        {
            item.status = new_status;
            if let Some(p) = path {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    item.file_name = name.to_string();
                }
                item.save_path = p;
            }
            return;
        }

        if let Some(item) = self
            .items
            .iter_mut()
            .find(|d| d.status == DownloadStatus::InProgress)
        {
            item.status = new_status;
            if let Some(p) = path {
                item.save_path = p;
            }
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

fn extract_filename_from_url(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last_segment = without_query.rsplit('/').next().unwrap_or("").trim();
    if last_segment.is_empty() || last_segment.contains(':') {
        String::from("download.bin")
    } else {
        sanitize_filename(last_segment)
    }
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
    let trimmed = out.trim().trim_matches('.');
    if trimmed.is_empty() {
        String::from("download.bin")
    } else {
        trimmed.to_string()
    }
}

fn unique_destination_path(downloads_dir: &Path, file_name: &str) -> PathBuf {
    let safe_name = sanitize_filename(file_name);
    let candidate = downloads_dir.join(&safe_name);
    if !candidate.exists() {
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
        if !next.exists() {
            return next;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
