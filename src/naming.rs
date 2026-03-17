use regex::Regex;
use std::path::{Path, PathBuf};
use tracing::instrument;

use crate::config::NamingConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Convert a filename to lowercase-hyphenated slug.
pub fn to_slug(filename: &str) -> String {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);

    let slug: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else if c == ' ' || c == '_' || c == '-' {
                '-'
            } else {
                // Drop non-alphanumeric, non-separator chars
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect();

    // Collapse multiple hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    result
}

/// Check if a filename matches lowercase-hyphenated convention.
fn is_valid_slug(stem: &str) -> bool {
    if stem.is_empty() {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !stem.starts_with('-')
        && !stem.ends_with('-')
        && !stem.contains("--")
}

/// Check if a path is exempt from naming rules.
fn is_exempt(path: &Path, exempt_patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    for pattern in exempt_patterns {
        if let Ok(re) = Regex::new(pattern)
            && re.is_match(&path_str)
        {
            return true;
        }
    }
    false
}

/// Run naming lint on all notes. Returns violations.
#[instrument(skip(notes, config))]
pub fn lint_naming(notes: &[Note], config: &NamingConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        if is_exempt(&note.path, &config.exempt_patterns) {
            continue;
        }

        let filename = match note.path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f,
            None => continue,
        };

        let stem = filename.strip_suffix(".md").unwrap_or(filename);

        // Check lowercase-hyphenated
        if !is_valid_slug(stem) {
            let suggested = to_slug(filename);
            let new_filename = format!("{suggested}.md");
            let new_path = note
                .path
                .parent()
                .map(|p| p.join(&new_filename))
                .unwrap_or_else(|| PathBuf::from(&new_filename));

            report.add(Violation {
                path: note.path.clone(),
                rule: "naming.lowercase-hyphenated".to_string(),
                severity: Severity::Error,
                message: format!("filename '{stem}' is not lowercase-hyphenated, suggest '{suggested}'"),
                fix: Some(Fix::RenameFile {
                    from: note.path.clone(),
                    to: new_path,
                }),
            });
        }

        // Check max length
        if stem.len() > config.max_length as usize {
            report.add(Violation {
                path: note.path.clone(),
                rule: "naming.max-length".to_string(),
                severity: Severity::Warning,
                message: format!("filename length {} exceeds max {}", stem.len(), config.max_length),
                fix: None,
            });
        }
    }

    tracing::info!(violation_count = report.violations.len(), "naming lint complete");
    report
}

/// Apply naming fixes: rename files and update wikilinks.
/// Returns a list of (old_path, new_path) renames performed.
#[instrument(skip(notes, config))]
pub fn apply_naming(vault_root: &Path, notes: &[Note], config: &NamingConfig) -> eyre::Result<Vec<(PathBuf, PathBuf)>> {
    let report = lint_naming(notes, config);
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Collect all renames first
    for violation in &report.violations {
        if let Some(Fix::RenameFile { from, to }) = &violation.fix {
            renames.push((from.clone(), to.clone()));
        }
    }

    if renames.is_empty() {
        return Ok(renames);
    }

    // Execute renames
    for (from, to) in &renames {
        let abs_from = vault_root.join(from);
        let abs_to = vault_root.join(to);

        if let Some(parent) = abs_to.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::rename(&abs_from, &abs_to)?;
        tracing::info!(from = %from.display(), to = %to.display(), "renamed file");
    }

    // Batch update all wikilinks across the vault
    update_wikilinks_batch(vault_root, notes, &renames)?;

    Ok(renames)
}

/// Update wikilinks in all vault files for a batch of renames.
/// Single pass through all files.
fn update_wikilinks_batch(vault_root: &Path, notes: &[Note], renames: &[(PathBuf, PathBuf)]) -> eyre::Result<()> {
    if renames.is_empty() {
        return Ok(());
    }

    // Build a map of old stem -> new stem (case-insensitive matching)
    let rename_map: Vec<(String, String)> = renames
        .iter()
        .filter_map(|(from, to)| {
            let old_stem = from.file_stem()?.to_str()?.to_string();
            let new_stem = to.file_stem()?.to_str()?.to_string();
            Some((old_stem, new_stem))
        })
        .collect();

    for note in notes {
        let abs_path = vault_root.join(&note.path);
        // Skip files that were renamed (they no longer exist at old path)
        if renames.iter().any(|(from, _)| *from == note.path) {
            continue;
        }

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut new_content = content.clone();

        for (old_stem, new_stem) in &rename_map {
            // Wikilinks are case-insensitive in Obsidian, match all case variants
            let pattern = format!(r"\[\[{}\]\]", regex::escape(old_stem));
            if let Ok(re) = Regex::new(&format!("(?i){pattern}")) {
                new_content = re.replace_all(&new_content, format!("[[{new_stem}]]")).to_string();
            }

            // Also handle [[link|display text]] format
            let pipe_pattern = format!(r"\[\[{}\|", regex::escape(old_stem));
            if let Ok(re) = Regex::new(&format!("(?i){pipe_pattern}")) {
                new_content = re.replace_all(&new_content, format!("[[{new_stem}|")).to_string();
            }
        }

        if new_content != content {
            std::fs::write(&abs_path, &new_content)?;
            tracing::info!(path = %note.path.display(), "updated wikilinks");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_slug_basic() {
        assert_eq!(to_slug("Hello World.md"), "hello-world");
        assert_eq!(to_slug("My_Note.md"), "my-note");
        assert_eq!(to_slug("already-valid.md"), "already-valid");
    }

    #[test]
    fn test_to_slug_special_chars() {
        assert_eq!(to_slug("Hello World!.md"), "hello-world");
        assert_eq!(to_slug("Test (1).md"), "test-1");
        assert_eq!(to_slug("A   B   C.md"), "a-b-c");
    }

    #[test]
    fn test_to_slug_preserves_numbers() {
        assert_eq!(to_slug("note-123.md"), "note-123");
        assert_eq!(to_slug("2026-03-16-daily.md"), "2026-03-16-daily");
    }

    #[test]
    fn test_is_valid_slug() {
        assert!(is_valid_slug("hello-world"));
        assert!(is_valid_slug("note-123"));
        assert!(is_valid_slug("a"));

        assert!(!is_valid_slug("Hello-World"));
        assert!(!is_valid_slug("hello_world"));
        assert!(!is_valid_slug("-leading"));
        assert!(!is_valid_slug("trailing-"));
        assert!(!is_valid_slug("double--hyphen"));
        assert!(!is_valid_slug(""));
    }

    #[test]
    fn test_lint_naming_reports_violations() {
        let notes = vec![
            Note {
                path: PathBuf::from("valid-note.md"),
                frontmatter: Default::default(),
                body: String::new(),
                raw: String::new(),
            },
            Note {
                path: PathBuf::from("Bad Note Name.md"),
                frontmatter: Default::default(),
                body: String::new(),
                raw: String::new(),
            },
        ];

        let config = NamingConfig {
            style: "lowercase-hyphenated".to_string(),
            max_length: 80,
            exempt_patterns: Vec::new(),
        };

        let report = lint_naming(&notes, &config);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule, "naming.lowercase-hyphenated");
    }

    #[test]
    fn test_lint_naming_max_length() {
        let long_name = format!("{}.md", "a".repeat(100));
        let notes = vec![Note {
            path: PathBuf::from(&long_name),
            frontmatter: Default::default(),
            body: String::new(),
            raw: String::new(),
        }];

        let config = NamingConfig {
            style: "lowercase-hyphenated".to_string(),
            max_length: 80,
            exempt_patterns: Vec::new(),
        };

        let report = lint_naming(&notes, &config);
        assert!(report.violations.iter().any(|v| v.rule == "naming.max-length"));
    }

    #[test]
    fn test_lint_naming_exempt() {
        let notes = vec![Note {
            path: PathBuf::from("System/Bad Name.md"),
            frontmatter: Default::default(),
            body: String::new(),
            raw: String::new(),
        }];

        let config = NamingConfig {
            style: "lowercase-hyphenated".to_string(),
            max_length: 80,
            exempt_patterns: vec!["^System/".to_string()],
        };

        let report = lint_naming(&notes, &config);
        assert!(report.is_empty());
    }
}
