use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::instrument;
use walkdir::WalkDir;

use crate::config::VaultConfig;

/// Parsed representation of a vault note.
#[derive(Debug, Clone)]
pub struct Note {
    /// Path relative to vault root.
    pub path: PathBuf,
    pub frontmatter: Frontmatter,
    /// Everything after the closing ---.
    pub body: String,
    /// Original file contents.
    pub raw: String,
}

/// Parsed frontmatter. Known fields extracted; everything else in extra.
/// We do NOT use #[serde(flatten)] with serde_yaml due to known issues.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    #[serde(rename = "type")]
    pub note_type: Option<String>,
    pub domain: Option<String>,
    pub origin: Option<String>,
    pub status: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source: Option<String>,
    pub creator: Option<String>,
    #[serde(skip)]
    pub extra: HashMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    /// Parse from a serde_yaml::Value (typically a Mapping).
    /// Known fields are extracted; everything else goes into extra.
    pub fn from_value(value: serde_yaml::Value) -> Result<Self> {
        let mapping = match value {
            serde_yaml::Value::Mapping(m) => m,
            _ => return Ok(Self::default()),
        };

        let mut title = None;
        let mut date = None;
        let mut note_type = None;
        let mut domain = None;
        let mut origin = None;
        let mut status = None;
        let mut tags = None;
        let mut source = None;
        let mut creator = None;
        let mut extra = HashMap::new();

        for (key, val) in mapping {
            let key_str = match &key {
                serde_yaml::Value::String(s) => s.clone(),
                _ => {
                    let s = format!("{key:?}");
                    extra.insert(s, val);
                    continue;
                }
            };

            match key_str.as_str() {
                "title" => {
                    title = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "date" => {
                    date = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "type" => {
                    note_type = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "domain" => {
                    domain = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "origin" => {
                    origin = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "status" => {
                    status = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "tags" => {
                    if let serde_yaml::Value::Sequence(seq) = val {
                        tags = Some(
                            seq.into_iter()
                                .filter_map(|v| match v {
                                    serde_yaml::Value::String(s) => Some(s),
                                    _ => None,
                                })
                                .collect(),
                        );
                    }
                }
                "source" => {
                    source = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                "creator" => {
                    creator = match val {
                        serde_yaml::Value::String(s) => Some(s),
                        other => Some(format!("{other:?}")),
                    };
                }
                _ => {
                    extra.insert(key_str, val);
                }
            }
        }

        Ok(Frontmatter {
            title,
            date,
            note_type,
            domain,
            origin,
            status,
            tags,
            source,
            creator,
            extra,
        })
    }

    /// Serialize back to YAML string, preserving extra fields.
    /// Fields emitted in canonical order: title, date, type, domain, origin, tags,
    /// status, source, creator, then extra fields alphabetically.
    pub fn to_yaml(&self) -> Result<String> {
        let mut mapping = serde_yaml::Mapping::new();

        if let Some(ref title) = self.title {
            mapping.insert(
                serde_yaml::Value::String("title".to_string()),
                serde_yaml::Value::String(title.clone()),
            );
        }
        if let Some(ref date) = self.date {
            mapping.insert(
                serde_yaml::Value::String("date".to_string()),
                serde_yaml::Value::String(date.clone()),
            );
        }
        if let Some(ref note_type) = self.note_type {
            mapping.insert(
                serde_yaml::Value::String("type".to_string()),
                serde_yaml::Value::String(note_type.clone()),
            );
        }
        if let Some(ref domain) = self.domain {
            mapping.insert(
                serde_yaml::Value::String("domain".to_string()),
                serde_yaml::Value::String(domain.clone()),
            );
        }
        if let Some(ref origin) = self.origin {
            mapping.insert(
                serde_yaml::Value::String("origin".to_string()),
                serde_yaml::Value::String(origin.clone()),
            );
        }
        if let Some(ref tags) = self.tags {
            let seq: Vec<serde_yaml::Value> = tags.iter().map(|t| serde_yaml::Value::String(t.clone())).collect();
            mapping.insert(
                serde_yaml::Value::String("tags".to_string()),
                serde_yaml::Value::Sequence(seq),
            );
        }
        if let Some(ref status) = self.status {
            mapping.insert(
                serde_yaml::Value::String("status".to_string()),
                serde_yaml::Value::String(status.clone()),
            );
        }
        if let Some(ref source) = self.source {
            mapping.insert(
                serde_yaml::Value::String("source".to_string()),
                serde_yaml::Value::String(source.clone()),
            );
        }
        if let Some(ref creator) = self.creator {
            mapping.insert(
                serde_yaml::Value::String("creator".to_string()),
                serde_yaml::Value::String(creator.clone()),
            );
        }

        // Add extra fields alphabetically
        let mut extra_keys: Vec<&String> = self.extra.keys().collect();
        extra_keys.sort();
        for key in extra_keys {
            if let Some(value) = self.extra.get(key) {
                mapping.insert(serde_yaml::Value::String(key.clone()), value.clone());
            }
        }

        let yaml =
            serde_yaml::to_string(&serde_yaml::Value::Mapping(mapping)).context("failed to serialize frontmatter")?;
        Ok(yaml)
    }

    /// Check if frontmatter is completely empty (no fields set).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.date.is_none()
            && self.note_type.is_none()
            && self.domain.is_none()
            && self.origin.is_none()
            && self.status.is_none()
            && self.tags.is_none()
            && self.source.is_none()
            && self.creator.is_none()
            && self.extra.is_empty()
    }
}

/// Parse a single markdown file into a Note.
pub fn parse_note(vault_root: &Path, path: &Path) -> Result<Note> {
    let raw = fs::read_to_string(path).context(format!("failed to read {}", path.display()))?;
    let relative = path.strip_prefix(vault_root).unwrap_or(path).to_path_buf();

    let (frontmatter, body) = parse_frontmatter(&raw)?;

    Ok(Note {
        path: relative,
        frontmatter,
        body,
        raw,
    })
}

/// Split raw markdown into frontmatter and body.
fn parse_frontmatter(raw: &str) -> Result<(Frontmatter, String)> {
    let trimmed = raw.trim_start();

    if !trimmed.starts_with("---") {
        // No frontmatter delimiters - entire file is body
        return Ok((Frontmatter::default(), raw.to_string()));
    }

    // Find closing ---
    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);

    if let Some(end_pos) = after_opening.find("\n---") {
        let yaml_str = &after_opening[..end_pos];
        let body_start = end_pos + 4; // skip \n---
        let body = after_opening[body_start..].trim_start_matches(['\r', '\n']).to_string();

        let value: serde_yaml::Value =
            serde_yaml::from_str(yaml_str).unwrap_or(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        let frontmatter = Frontmatter::from_value(value)?;

        Ok((frontmatter, body))
    } else {
        // Opening --- but no closing --- - treat as no frontmatter
        Ok((Frontmatter::default(), raw.to_string()))
    }
}

/// Scan an entire vault and return all parsed notes.
#[instrument(skip(vault_config), fields(vault_root = %vault_root.display()))]
pub fn scan_vault(vault_root: &Path, vault_config: &VaultConfig) -> Result<Vec<Note>> {
    let mut notes = Vec::new();

    for entry in WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                return !vault_config.ignore.iter().any(|ig| name == *ig);
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

        match parse_note(vault_root, path) {
            Ok(note) => notes.push(note),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "failed to parse note");
            }
        }
    }

    notes.sort_by(|a, b| a.path.cmp(&b.path));
    tracing::info!(note_count = notes.len(), "vault parsed");

    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestVault;

    #[test]
    fn test_parse_note_with_frontmatter() {
        let v = TestVault::new();
        let note = parse_note(v.root(), &v.root().join("rust-guide.md")).expect("parse");
        assert_eq!(note.frontmatter.title.as_deref(), Some("Rust Guide"));
        assert_eq!(note.frontmatter.date.as_deref(), Some("2026-03-10"));
        assert_eq!(note.frontmatter.note_type.as_deref(), Some("note"));
        assert_eq!(
            note.frontmatter.tags,
            Some(vec!["rust".to_string(), "programming".to_string()])
        );
        assert!(note.body.contains("A guide to Rust programming."));
    }

    #[test]
    fn test_parse_note_without_frontmatter() {
        let v = TestVault::new();
        let note = parse_note(v.root(), &v.root().join("bare-note.md")).expect("parse");
        assert!(note.frontmatter.is_empty());
        assert!(note.body.contains("Just some text"));
    }

    #[test]
    fn test_parse_note_with_extra_fields() {
        let v = TestVault::new();
        let note = parse_note(v.root(), &v.root().join("cool-video.md")).expect("parse");
        assert_eq!(note.frontmatter.title.as_deref(), Some("Cool Video"));
        assert_eq!(note.frontmatter.note_type.as_deref(), Some("video"));
    }

    #[test]
    fn test_frontmatter_roundtrip() {
        let fm = Frontmatter {
            title: Some("Test".to_string()),
            date: Some("2026-01-01".to_string()),
            note_type: Some("note".to_string()),
            domain: Some("tech".to_string()),
            origin: Some("authored".to_string()),
            tags: Some(vec!["rust".to_string()]),
            ..Default::default()
        };

        let yaml = fm.to_yaml().expect("to_yaml");
        assert!(yaml.contains("title: Test"));
        assert!(yaml.contains("date: '2026-01-01'") || yaml.contains("date: 2026-01-01"));
    }

    #[test]
    fn test_scan_vault_ignores_obsidian_dir() {
        let v = TestVault::new();
        let notes = v.scan();
        // .obsidian/workspace.md should NOT appear
        assert!(!notes.iter().any(|n| n.path.to_string_lossy().contains(".obsidian")));
    }

    #[test]
    fn test_scan_vault_includes_system_files() {
        let v = TestVault::new();
        let notes = v.scan();
        // system/ files are now scanned (exclude/include is enforcement-level, not scan-level)
        assert!(notes.iter().any(|n| n.path.to_string_lossy().contains("borg-ledger")));
    }

    #[test]
    fn test_scan_vault_finds_all_non_ignored() {
        let v = TestVault::new();
        let notes = v.scan();
        // Should find all .md files except .obsidian/* (ignored dirs)
        assert!(notes.iter().any(|n| n.path == Path::new("rust-guide.md")));
        assert!(notes.iter().any(|n| n.path == Path::new("bare-note.md")));
        assert!(notes.iter().any(|n| n.path == Path::new("projects/obsidian-cortex.md")));
        // readme.txt should not appear (not .md)
        assert!(!notes.iter().any(|n| n.path.to_string_lossy().contains("readme")));
    }
}
