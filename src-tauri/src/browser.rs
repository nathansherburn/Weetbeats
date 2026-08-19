//! Finding samples on disk.

use std::path::{Path, PathBuf};

use serde::Serialize;
use weetbeats_engine::sample::is_audio_file;

/// How deep into a folder we will look. Sample packs nest a couple of levels; nobody
/// wants their whole home folder walked.
const MAX_DEPTH: usize = 4;

/// Files we will list from one folder. A wall of ten thousand names is not a browser.
const MAX_FILES: usize = 2_000;

/// One row in the sample list. Deliberately does not decode anything: scanning a folder
/// has to feel instant, and a sample is only read when it is clicked or dragged.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleEntry {
    pub path: String,
    pub name: String,
    /// Folder relative to the one that was picked, so nested packs still make sense.
    pub folder: String,
}

/// What a scan found.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub root: String,
    pub entries: Vec<SampleEntry>,
    /// True when there were more files than we were willing to list.
    pub truncated: bool,
}

/// Walk a folder for anything that looks like audio.
pub fn scan(root: &Path) -> Listing {
    let mut entries = Vec::new();
    let mut truncated = false;
    walk(root, root, 0, &mut entries, &mut truncated);

    // Folder first, then name, so a pack's own layout survives the flattening.
    entries.sort_by(|a, b| {
        a.folder
            .cmp(&b.folder)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Listing {
        root: root.display().to_string(),
        entries,
        truncated,
    }
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    entries: &mut Vec<SampleEntry>,
    truncated: &mut bool,
) {
    if depth > MAX_DEPTH || entries.len() >= MAX_FILES {
        *truncated = *truncated || entries.len() >= MAX_FILES;
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };

    // Collect and sort so the same folder always lists in the same order; read_dir is
    // whatever order the filesystem feels like.
    let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if entries.len() >= MAX_FILES {
            *truncated = true;
            return;
        }
        // Skip hidden files, and the dot folders that come with sample packs.
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(true);
        if hidden {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, depth + 1, entries, truncated);
        } else if is_audio_file(&path) {
            entries.push(SampleEntry {
                path: path.display().to_string(),
                name: path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sample")
                    .to_string(),
                folder: path
                    .parent()
                    .and_then(|p| p.strip_prefix(root).ok())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            });
        }
    }
}
