//! Small shared helpers: content hashing, human-readable durations, and
//! discovering resume files in a folder.

use crate::ingest;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// SHA-256 of a file's bytes, hex-encoded. Used as the cache/dedupe key so an
/// unchanged file is never re-processed.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Recursively collect all supported resume files under `root`, sorted for
/// stable ordering.
pub fn discover_resumes(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| ingest::is_supported(p))
        // skip macOS resource-fork/hidden junk
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with("._") && !n.starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// Render a month count as "2 yr 3 mo" / "5 mo" / "—".
pub fn humanize_months(months: i64) -> String {
    if months <= 0 {
        return "—".to_string();
    }
    let years = months / 12;
    let rem = months % 12;
    match (years, rem) {
        (0, m) => format!("{m} mo"),
        (y, 0) => format!("{y} yr"),
        (y, m) => format!("{y} yr {m} mo"),
    }
}
