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
    pub tags: Option<Vec<String>>,
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
        let mut tags = None;
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
                _ => {
                    extra.insert(key_str, val);
                }
            }
        }

        Ok(Frontmatter {
            title,
            date,
            note_type,
            tags,
            extra,
        })
    }

    /// Serialize back to YAML string, preserving extra fields.
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
        if let Some(ref tags) = self.tags {
            let seq: Vec<serde_yaml::Value> = tags.iter().map(|t| serde_yaml::Value::String(t.clone())).collect();
            mapping.insert(
                serde_yaml::Value::String("tags".to_string()),
                serde_yaml::Value::Sequence(seq),
            );
        }

        // Add extra fields
        for (key, value) in &self.extra {
            mapping.insert(serde_yaml::Value::String(key.clone()), value.clone());
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
            && self.tags.is_none()
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

        let relative = path.strip_prefix(vault_root).unwrap_or(path);

        // Skip protected files
        let relative_str = relative.to_string_lossy();
        if vault_config.protected.iter().any(|p| relative_str == *p) {
            tracing::debug!(path = %relative_str, "skipping protected file");
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
    use std::io::Write;

    fn write_note(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        let mut f = fs::File::create(path).expect("create file");
        write!(f, "{content}").expect("write");
    }

    #[test]
    fn test_parse_note_with_frontmatter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_note(
            tmp.path(),
            "test.md",
            "---\ntitle: Hello\ndate: 2026-01-01\ntype: note\ntags:\n  - rust\n  - cli\n---\nBody text here.\n",
        );

        let note = parse_note(tmp.path(), &tmp.path().join("test.md")).expect("parse");
        assert_eq!(note.frontmatter.title.as_deref(), Some("Hello"));
        assert_eq!(note.frontmatter.date.as_deref(), Some("2026-01-01"));
        assert_eq!(note.frontmatter.note_type.as_deref(), Some("note"));
        assert_eq!(note.frontmatter.tags, Some(vec!["rust".to_string(), "cli".to_string()]));
        assert!(note.body.contains("Body text here."));
    }

    #[test]
    fn test_parse_note_without_frontmatter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_note(tmp.path(), "bare.md", "Just some text without frontmatter.\n");

        let note = parse_note(tmp.path(), &tmp.path().join("bare.md")).expect("parse");
        assert!(note.frontmatter.is_empty());
        assert!(note.body.contains("Just some text"));
    }

    #[test]
    fn test_parse_note_with_extra_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_note(
            tmp.path(),
            "extra.md",
            "---\ntitle: Extra\ndate: 2026-01-01\ntype: video\ntags: []\nsource: youtube\nchannel: test\n---\nVideo notes.\n",
        );

        let note = parse_note(tmp.path(), &tmp.path().join("extra.md")).expect("parse");
        assert_eq!(note.frontmatter.title.as_deref(), Some("Extra"));
        assert!(note.frontmatter.extra.contains_key("source"));
        assert!(note.frontmatter.extra.contains_key("channel"));
    }

    #[test]
    fn test_frontmatter_roundtrip() {
        let fm = Frontmatter {
            title: Some("Test".to_string()),
            date: Some("2026-01-01".to_string()),
            note_type: Some("note".to_string()),
            tags: Some(vec!["rust".to_string()]),
            extra: HashMap::new(),
        };

        let yaml = fm.to_yaml().expect("to_yaml");
        assert!(yaml.contains("title: Test"));
        assert!(yaml.contains("date: '2026-01-01'") || yaml.contains("date: 2026-01-01"));
    }

    #[test]
    fn test_scan_vault() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_note(tmp.path(), "note1.md", "---\ntitle: Note 1\n---\nFirst note.\n");
        write_note(tmp.path(), "note2.md", "---\ntitle: Note 2\n---\nSecond note.\n");
        write_note(
            tmp.path(),
            ".obsidian/config.md",
            "---\ntitle: Config\n---\nShould be ignored.\n",
        );

        let config = VaultConfig {
            root_path: None,
            ignore: vec![".obsidian".to_string()],
            protected: Vec::new(),
        };

        let notes = scan_vault(tmp.path(), &config).expect("scan");
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn test_scan_vault_skips_protected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_note(tmp.path(), "normal.md", "---\ntitle: Normal\n---\nKeep.\n");
        write_note(tmp.path(), "protected.md", "---\ntitle: Protected\n---\nSkip.\n");

        let config = VaultConfig {
            root_path: None,
            ignore: Vec::new(),
            protected: vec!["protected.md".to_string()],
        };

        let notes = scan_vault(tmp.path(), &config).expect("scan");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].path, PathBuf::from("normal.md"));
    }
}
