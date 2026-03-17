use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::instrument;
use walkdir::WalkDir;

/// Per-file metadata for change detection (no content read needed).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: u64,
}

/// Manifest of the entire vault from the last run.
#[derive(Debug, Serialize, Deserialize)]
pub struct VaultManifest {
    pub timestamp: DateTime<Utc>,
    pub files: Vec<FileEntry>,
}

/// Changes detected between two manifests.
#[derive(Debug, Default)]
pub struct ManifestDiff {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
}

impl ManifestDiff {
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.removed.is_empty() || !self.modified.is_empty()
    }
}

impl VaultManifest {
    /// Scan the vault and build a fresh manifest.
    #[instrument(skip(ignore_dirs), fields(vault_root = %vault_root.display()))]
    pub fn scan(vault_root: &Path, ignore_dirs: &[String]) -> Result<Self> {
        let mut files = Vec::new();

        for entry in WalkDir::new(vault_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    return !ignore_dirs.iter().any(|ig| name == *ig);
                }
                true
            })
        {
            let entry = entry.context("failed to read directory entry")?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            let metadata = entry.metadata().context("failed to read file metadata")?;
            let mtime = metadata
                .modified()
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0);

            let relative = path.strip_prefix(vault_root).unwrap_or(path).to_path_buf();

            files.push(FileEntry {
                path: relative,
                size: metadata.len(),
                mtime,
            });
        }

        files.sort_by(|a, b| a.path.cmp(&b.path));

        tracing::info!(file_count = files.len(), "vault scan complete");

        Ok(VaultManifest {
            timestamp: Utc::now(),
            files,
        })
    }

    /// Compute the diff between this manifest (old) and another (new).
    pub fn diff(&self, newer: &VaultManifest) -> ManifestDiff {
        let old_map: HashMap<&PathBuf, &FileEntry> = self.files.iter().map(|f| (&f.path, f)).collect();
        let new_map: HashMap<&PathBuf, &FileEntry> = newer.files.iter().map(|f| (&f.path, f)).collect();

        let mut diff = ManifestDiff::default();

        for (path, new_entry) in &new_map {
            match old_map.get(path) {
                Some(old_entry) => {
                    if old_entry.size != new_entry.size || old_entry.mtime != new_entry.mtime {
                        diff.modified.push((*path).clone());
                    }
                }
                None => {
                    diff.added.push((*path).clone());
                }
            }
        }

        for path in old_map.keys() {
            if !new_map.contains_key(path) {
                diff.removed.push((*path).clone());
            }
        }

        diff.added.sort();
        diff.removed.sort();
        diff.modified.sort();

        diff
    }

    /// Load a cached manifest from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).context("failed to read manifest")?;
        let manifest: Self = serde_yaml::from_str(&content).context("failed to parse manifest")?;
        Ok(manifest)
    }

    /// Save this manifest to disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create manifest directory")?;
        }
        let content = serde_yaml::to_string(self).context("failed to serialize manifest")?;
        fs::write(path, content).context("failed to write manifest")?;
        tracing::info!(path = %path.display(), "manifest saved");
        Ok(())
    }

    /// Path to the manifest file for a given vault root and cache dir.
    pub fn manifest_path(vault_root: &Path, cache_dir: &str) -> PathBuf {
        vault_root.join(cache_dir).join("manifest.yml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn create_test_vault(dir: &Path) {
        let note1 = dir.join("note1.md");
        let note2 = dir.join("note2.md");
        let sub = dir.join("subfolder");
        fs::create_dir_all(&sub).expect("create subfolder");
        let note3 = sub.join("note3.md");

        let mut f1 = fs::File::create(note1).expect("create note1");
        writeln!(f1, "---\ntitle: Note 1\n---\nHello").expect("write note1");

        let mut f2 = fs::File::create(note2).expect("create note2");
        writeln!(f2, "---\ntitle: Note 2\n---\nWorld").expect("write note2");

        let mut f3 = fs::File::create(note3).expect("create note3");
        writeln!(f3, "---\ntitle: Note 3\n---\nSub note").expect("write note3");

        // Non-md file should be ignored
        fs::write(dir.join("readme.txt"), "not a note").expect("write txt");
    }

    #[test]
    fn test_scan_finds_md_files() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        create_test_vault(tmp.path());

        let manifest = VaultManifest::scan(tmp.path(), &[]).expect("scan");
        assert_eq!(manifest.files.len(), 3);
    }

    #[test]
    fn test_scan_ignores_directories() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        create_test_vault(tmp.path());

        let manifest = VaultManifest::scan(tmp.path(), &["subfolder".to_string()]).expect("scan");
        assert_eq!(manifest.files.len(), 2);
    }

    #[test]
    fn test_diff_detects_added() {
        let old = VaultManifest {
            timestamp: Utc::now(),
            files: vec![FileEntry {
                path: PathBuf::from("a.md"),
                size: 10,
                mtime: 100,
            }],
        };
        let new = VaultManifest {
            timestamp: Utc::now(),
            files: vec![
                FileEntry {
                    path: PathBuf::from("a.md"),
                    size: 10,
                    mtime: 100,
                },
                FileEntry {
                    path: PathBuf::from("b.md"),
                    size: 20,
                    mtime: 200,
                },
            ],
        };

        let diff = old.diff(&new);
        assert_eq!(diff.added, vec![PathBuf::from("b.md")]);
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn test_diff_detects_removed() {
        let old = VaultManifest {
            timestamp: Utc::now(),
            files: vec![
                FileEntry {
                    path: PathBuf::from("a.md"),
                    size: 10,
                    mtime: 100,
                },
                FileEntry {
                    path: PathBuf::from("b.md"),
                    size: 20,
                    mtime: 200,
                },
            ],
        };
        let new = VaultManifest {
            timestamp: Utc::now(),
            files: vec![FileEntry {
                path: PathBuf::from("a.md"),
                size: 10,
                mtime: 100,
            }],
        };

        let diff = old.diff(&new);
        assert!(diff.added.is_empty());
        assert_eq!(diff.removed, vec![PathBuf::from("b.md")]);
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn test_diff_detects_modified() {
        let old = VaultManifest {
            timestamp: Utc::now(),
            files: vec![FileEntry {
                path: PathBuf::from("a.md"),
                size: 10,
                mtime: 100,
            }],
        };
        let new = VaultManifest {
            timestamp: Utc::now(),
            files: vec![FileEntry {
                path: PathBuf::from("a.md"),
                size: 15,
                mtime: 200,
            }],
        };

        let diff = old.diff(&new);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert_eq!(diff.modified, vec![PathBuf::from("a.md")]);
    }

    #[test]
    fn test_manifest_roundtrip() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let path = tmp.path().join("manifest.yml");

        let manifest = VaultManifest {
            timestamp: Utc::now(),
            files: vec![FileEntry {
                path: PathBuf::from("test.md"),
                size: 42,
                mtime: 1234567890,
            }],
        };

        manifest.save(&path).expect("save");
        let loaded = VaultManifest::load(&path).expect("load");
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[0].path, PathBuf::from("test.md"));
        assert_eq!(loaded.files[0].size, 42);
    }
}
