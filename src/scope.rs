use std::path::Path;
use tracing::instrument;

use crate::config::ScopeConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Run scope classification on all notes.
#[instrument(skip(notes, config))]
pub fn lint_scope(notes: &[Note], config: &ScopeConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        for rule in &config.rules {
            if matches_rule(note, rule) {
                // Check if the note already has the scope fields set correctly
                for (key, value) in &rule.set {
                    let current = note.frontmatter.extra.get(key);

                    if current != Some(value) {
                        report.add(Violation {
                            path: note.path.clone(),
                            rule: format!("scope.{key}"),
                            severity: Severity::Warning,
                            message: format!("scope rule matched: should set {key}={value:?}"),
                            fix: Some(Fix::SetFrontmatter {
                                key: key.clone(),
                                value: value.clone(),
                            }),
                        });
                    }
                }
            }
        }
    }

    tracing::info!(violation_count = report.violations.len(), "scope lint complete");
    report
}

/// Apply scope fixes: set frontmatter fields.
#[instrument(skip(notes, config))]
pub fn apply_scope(vault_root: &Path, notes: &[Note], config: &ScopeConfig) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let mut fields_to_set: Vec<(String, serde_yaml::Value)> = Vec::new();

        for rule in &config.rules {
            if matches_rule(note, rule) {
                for (key, value) in &rule.set {
                    let current = note.frontmatter.extra.get(key);
                    if current != Some(value) {
                        fields_to_set.push((key.clone(), value.clone()));
                    }
                }
            }
        }

        if fields_to_set.is_empty() {
            continue;
        }

        let abs_path = vault_root.join(&note.path);
        let content = std::fs::read_to_string(&abs_path)?;

        if let Some(new_content) = insert_frontmatter_fields(&content, &fields_to_set) {
            std::fs::write(&abs_path, new_content)?;
            tracing::info!(path = %note.path.display(), "applied scope fields");
            fixed_count += 1;
        }
    }

    Ok(fixed_count)
}

fn matches_rule(note: &Note, rule: &crate::config::ScopeRule) -> bool {
    let match_criteria = &rule.match_criteria;

    // Check tag-based matching
    if let Some(ref match_tags) = match_criteria.tags
        && let Some(ref note_tags) = note.frontmatter.tags
    {
        let has_match = match_tags.iter().any(|mt| note_tags.iter().any(|nt| nt == mt));
        if has_match {
            return true;
        }
    }

    // Check source-contains matching
    if let Some(ref source_pattern) = match_criteria.source_contains
        && let Some(serde_yaml::Value::String(source)) = note.frontmatter.extra.get("source")
        && source.to_lowercase().contains(&source_pattern.to_lowercase())
    {
        return true;
    }

    false
}

/// Insert key-value pairs into frontmatter before the closing ---.
pub fn insert_frontmatter_fields(content: &str, fields: &[(String, serde_yaml::Value)]) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);
    let end_pos = after_opening.find("\n---")?;

    let fm_block = &after_opening[..end_pos];
    let rest = &after_opening[end_pos..];

    let mut new_lines: Vec<String> = fm_block.lines().map(String::from).collect();

    for (key, value) in fields {
        // Remove existing line for this key if present
        new_lines.retain(|line| !line.starts_with(&format!("{key}:")));

        // Add new line
        let value_str = match value {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Bool(b) => b.to_string(),
            serde_yaml::Value::Number(n) => n.to_string(),
            other => format!("{other:?}"),
        };
        new_lines.push(format!("{key}: {value_str}"));
    }

    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    let new_fm = new_lines.join("\n");

    Some(format!("{prefix}---\n{new_fm}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScopeMatch, ScopeRule};
    use crate::vault::Frontmatter;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_note(path: &str, tags: Vec<&str>, extra: HashMap<String, serde_yaml::Value>) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter {
                title: Some("Test".to_string()),
                date: Some("2026-01-01".to_string()),
                note_type: Some("note".to_string()),
                tags: Some(tags.into_iter().map(String::from).collect()),
                extra,
            },
            body: String::new(),
            raw: String::new(),
        }
    }

    fn work_config() -> ScopeConfig {
        ScopeConfig {
            rules: vec![ScopeRule {
                match_criteria: ScopeMatch {
                    tags: Some(vec!["sre".to_string(), "tatari".to_string()]),
                    source_contains: None,
                },
                set: {
                    let mut m = HashMap::new();
                    m.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));
                    m.insert("company".to_string(), serde_yaml::Value::String("tatari".to_string()));
                    m
                },
            }],
        }
    }

    #[test]
    fn test_scope_matches_by_tag() {
        let note = make_note("work-note.md", vec!["sre", "kubernetes"], HashMap::new());
        let report = lint_scope(&[note], &work_config());
        assert!(!report.is_empty());
        assert!(report.violations.iter().any(|v| v.rule == "scope.scope"));
    }

    #[test]
    fn test_scope_no_match() {
        let note = make_note("personal.md", vec!["rust", "hobby"], HashMap::new());
        let report = lint_scope(&[note], &work_config());
        assert!(report.is_empty());
    }

    #[test]
    fn test_scope_already_set() {
        let mut extra = HashMap::new();
        extra.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));
        extra.insert("company".to_string(), serde_yaml::Value::String("tatari".to_string()));

        let note = make_note("already-scoped.md", vec!["sre"], extra);
        let report = lint_scope(&[note], &work_config());
        assert!(report.is_empty());
    }

    #[test]
    fn test_scope_source_contains() {
        let config = ScopeConfig {
            rules: vec![ScopeRule {
                match_criteria: ScopeMatch {
                    tags: None,
                    source_contains: Some("granola".to_string()),
                },
                set: {
                    let mut m = HashMap::new();
                    m.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));
                    m.insert("confidential".to_string(), serde_yaml::Value::Bool(true));
                    m
                },
            }],
        };

        let mut extra = HashMap::new();
        extra.insert(
            "source".to_string(),
            serde_yaml::Value::String("granola-meeting-notes".to_string()),
        );

        let note = make_note("meeting.md", vec![], extra);
        let report = lint_scope(&[note], &config);
        assert!(!report.is_empty());
    }

    #[test]
    fn test_insert_frontmatter_fields() {
        let content = "---\ntitle: Test\ndate: 2026-01-01\n---\nBody\n";
        let fields = vec![
            ("scope".to_string(), serde_yaml::Value::String("work".to_string())),
            ("company".to_string(), serde_yaml::Value::String("tatari".to_string())),
        ];

        let result = insert_frontmatter_fields(content, &fields);
        assert!(result.is_some());
        let result = result.expect("should have result");
        assert!(result.contains("scope: work"));
        assert!(result.contains("company: tatari"));
        assert!(result.contains("title: Test"));
    }
}
