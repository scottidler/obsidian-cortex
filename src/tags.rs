use std::collections::HashMap;
use std::path::Path;
use tracing::instrument;

use crate::config::TagsConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Run tag normalization lint on all notes.
#[instrument(skip(notes, config))]
pub fn lint_tags(notes: &[Note], config: &TagsConfig) -> Report {
    let mut report = Report::default();
    let mut tag_usage: HashMap<String, usize> = HashMap::new();

    for note in notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_usage.entry(tag.clone()).or_insert(0) += 1;

                // Check if tag is an alias
                if let Some(canonical) = config.aliases.get(tag) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.alias".to_string(),
                        severity: Severity::Warning,
                        message: format!("tag '{tag}' is an alias for '{canonical}'"),
                        fix: Some(Fix::ReplaceTag {
                            old: tag.clone(),
                            new: canonical.clone(),
                        }),
                    });
                    continue;
                }

                // Check if tag is lowercase-hyphenated
                if !is_valid_tag(tag) {
                    let normalized = normalize_tag(tag);
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.format".to_string(),
                        severity: Severity::Warning,
                        message: format!("tag '{tag}' is not lowercase-hyphenated"),
                        fix: Some(Fix::ReplaceTag {
                            old: tag.clone(),
                            new: normalized,
                        }),
                    });
                    continue;
                }

                // Check if tag is in canonical list (if list is non-empty)
                if !config.canonical.is_empty() && !config.canonical.contains(tag) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.non-canonical".to_string(),
                        severity: Severity::Info,
                        message: format!("tag '{tag}' is not in canonical list"),
                        fix: None,
                    });
                }
            }
        }
    }

    // Report orphan tags (used by only one note)
    for (tag, count) in &tag_usage {
        if *count == 1 {
            report.add(Violation {
                path: std::path::PathBuf::from("(vault-wide)"),
                rule: "tags.orphan".to_string(),
                severity: Severity::Info,
                message: format!("tag '{tag}' is used by only 1 note"),
                fix: None,
            });
        }
    }

    tracing::info!(violation_count = report.violations.len(), "tags lint complete");
    report
}

/// Apply tag fixes: rewrite tag lists in frontmatter.
#[instrument(skip(notes, config))]
pub fn apply_tags(vault_root: &Path, notes: &[Note], config: &TagsConfig) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let tags = match &note.frontmatter.tags {
            Some(t) => t,
            None => continue,
        };

        let mut new_tags = tags.clone();
        let mut changed = false;

        for (i, tag) in tags.iter().enumerate() {
            // Resolve alias
            if let Some(canonical) = config.aliases.get(tag) {
                new_tags[i] = canonical.clone();
                changed = true;
                continue;
            }

            // Normalize format
            if !is_valid_tag(tag) {
                new_tags[i] = normalize_tag(tag);
                changed = true;
            }
        }

        // Deduplicate tags after normalization
        new_tags.sort();
        new_tags.dedup();
        if new_tags.len() != tags.len() {
            changed = true;
        }

        if changed {
            let abs_path = vault_root.join(&note.path);
            let content = std::fs::read_to_string(&abs_path)?;

            if let Some(new_content) = replace_tags_in_frontmatter(&content, &new_tags) {
                std::fs::write(&abs_path, new_content)?;
                tracing::info!(path = %note.path.display(), "updated tags");
                fixed_count += 1;
            }
        }
    }

    Ok(fixed_count)
}

/// Check if a tag is valid lowercase-hyphenated format.
fn is_valid_tag(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    tag.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !tag.starts_with('-')
        && !tag.ends_with('-')
        && !tag.contains("--")
}

/// Normalize a tag to lowercase-hyphenated format.
fn normalize_tag(tag: &str) -> String {
    tag.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Replace the tags line in frontmatter YAML with new tags.
/// Uses targeted string replacement to avoid full re-serialization.
fn replace_tags_in_frontmatter(content: &str, new_tags: &[String]) -> Option<String> {
    // Find the frontmatter block
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);
    let end_pos = after_opening.find("\n---")?;
    let fm_block = &after_opening[..end_pos];

    // Find the tags line(s) in frontmatter
    let lines: Vec<&str> = fm_block.lines().collect();
    let mut new_lines: Vec<String> = Vec::new();
    let mut in_tags = false;

    for line in &lines {
        if line.starts_with("tags:") {
            in_tags = true;
            // Replace with inline format
            let tags_str: Vec<String> = new_tags.iter().map(|t| t.to_string()).collect();
            new_lines.push(format!("tags: [{}]", tags_str.join(", ")));
            // Check if it's inline already
            if line.contains('[') {
                in_tags = false;
            }
        } else if in_tags {
            // Skip continuation lines (  - tag)
            if line.starts_with("  -") || line.starts_with("  ") && line.trim().starts_with('-') {
                continue;
            } else {
                in_tags = false;
                new_lines.push(line.to_string());
            }
        } else {
            new_lines.push(line.to_string());
        }
    }

    let new_fm = new_lines.join("\n");
    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    let fm_start = 3 + (after_opening.as_ptr() as usize - trimmed[3..].as_ptr() as usize);
    let after_fm = &trimmed[fm_start + end_pos..];

    Some(format!("{prefix}---\n{new_fm}{after_fm}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::Frontmatter;
    use std::path::PathBuf;

    fn make_note(path: &str, tags: Vec<&str>) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter {
                title: Some("Test".to_string()),
                date: Some("2026-01-01".to_string()),
                note_type: Some("note".to_string()),
                tags: Some(tags.into_iter().map(String::from).collect()),
                extra: Default::default(),
            },
            body: String::new(),
            raw: String::new(),
        }
    }

    fn default_config() -> TagsConfig {
        TagsConfig {
            style: "lowercase-hyphenated".to_string(),
            canonical: vec![
                "ai-llm".to_string(),
                "rust".to_string(),
                "python".to_string(),
                "kubernetes".to_string(),
            ],
            aliases: {
                let mut m = HashMap::new();
                m.insert("ai".to_string(), "ai-llm".to_string());
                m.insert("k8s".to_string(), "kubernetes".to_string());
                m
            },
        }
    }

    #[test]
    fn test_valid_tags() {
        let note = make_note("valid.md", vec!["rust", "python"]);
        let report = lint_tags(&[note], &default_config());
        // Only orphan warnings expected
        let non_orphan: Vec<_> = report.violations.iter().filter(|v| v.rule != "tags.orphan").collect();
        assert!(non_orphan.is_empty());
    }

    #[test]
    fn test_alias_resolution() {
        let note = make_note("alias.md", vec!["ai", "rust"]);
        let report = lint_tags(&[note], &default_config());
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule == "tags.alias" && v.message.contains("ai-llm"))
        );
    }

    #[test]
    fn test_non_canonical_tag() {
        let note = make_note("non-canon.md", vec!["obscure-tag"]);
        let report = lint_tags(&[note], &default_config());
        assert!(report.violations.iter().any(|v| v.rule == "tags.non-canonical"));
    }

    #[test]
    fn test_invalid_format() {
        let note = make_note("bad-format.md", vec!["Bad Tag"]);
        let report = lint_tags(&[note], &default_config());
        assert!(report.violations.iter().any(|v| v.rule == "tags.format"));
    }

    #[test]
    fn test_normalize_tag() {
        assert_eq!(normalize_tag("Hello World"), "hello-world");
        assert_eq!(normalize_tag("AI/ML"), "ai-ml");
        assert_eq!(normalize_tag("already-valid"), "already-valid");
        assert_eq!(normalize_tag("UPPERCASE"), "uppercase");
    }

    #[test]
    fn test_is_valid_tag() {
        assert!(is_valid_tag("rust"));
        assert!(is_valid_tag("ai-llm"));
        assert!(is_valid_tag("k8s"));

        assert!(!is_valid_tag("Bad"));
        assert!(!is_valid_tag("has space"));
        assert!(!is_valid_tag("-leading"));
        assert!(!is_valid_tag(""));
    }

    #[test]
    fn test_orphan_detection() {
        let notes = vec![
            make_note("a.md", vec!["rust", "python"]),
            make_note("b.md", vec!["rust"]),
        ];

        let report = lint_tags(&notes, &default_config());
        let orphans: Vec<_> = report.violations.iter().filter(|v| v.rule == "tags.orphan").collect();
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].message.contains("python"));
    }

    #[test]
    fn test_replace_tags_in_frontmatter() {
        let content = "---\ntitle: Test\ntags: [old-tag, bad]\ndate: 2026-01-01\n---\nBody\n";
        let new_tags = vec!["new-tag".to_string(), "good".to_string()];
        let result = replace_tags_in_frontmatter(content, &new_tags);
        assert!(result.is_some());
        let result = result.expect("should have result");
        assert!(result.contains("tags: [new-tag, good]"));
        assert!(result.contains("title: Test"));
    }
}
