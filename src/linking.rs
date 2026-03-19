use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;
use tracing::instrument;

use crate::config::LinkingConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Regex to find existing wikilinks (to avoid double-linking).
static EXISTING_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Run wikilink inference on all notes.
#[instrument(skip(notes, config))]
pub fn lint_linking(notes: &[Note], config: &LinkingConfig) -> Report {
    let mut report = Report::default();

    // Build entity lists from config + note titles
    let note_titles: Vec<(String, String)> = notes
        .iter()
        .filter_map(|n| {
            let stem = n.path.file_stem()?.to_str()?.to_string();
            let title = n.frontmatter.title.clone().unwrap_or_else(|| stem.clone());
            Some((stem, title))
        })
        .collect();

    let scan_for: HashSet<&str> = config.scan_for.iter().map(|s| s.as_str()).collect();

    for note in notes {
        let existing_links = extract_existing_links(&note.body);

        // Match note titles/stems in body text
        if scan_for.contains("concepts") || scan_for.contains("all") {
            for (stem, title) in &note_titles {
                // Don't self-link
                if note.path.file_stem().and_then(|s| s.to_str()) == Some(stem) {
                    continue;
                }

                // Don't suggest if already linked
                if existing_links.contains(&stem.to_lowercase()) {
                    continue;
                }

                // Check if the title or stem appears in the body (case-insensitive)
                if let Some(context) = find_mention(&note.body, title, stem) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.concept".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{title}' could be linked as [[{stem}]]"),
                        fix: Some(Fix::AddWikilink {
                            target: stem.clone(),
                            context,
                        }),
                    });
                }
            }
        }

        // Match known people entities
        if scan_for.contains("people") || scan_for.contains("all") {
            for person in &config.entities.people {
                if existing_links.contains(&person.to_lowercase()) {
                    continue;
                }
                if let Some(context) = find_mention(&note.body, person, person) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.person".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{person}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: person.clone(),
                            context,
                        }),
                    });
                }
            }
        }

        // Match known project entities
        if scan_for.contains("projects") || scan_for.contains("all") {
            for project in &config.entities.projects {
                if existing_links.contains(&project.to_lowercase()) {
                    continue;
                }
                if let Some(context) = find_mention(&note.body, project, project) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "linking.project".to_string(),
                        severity: Severity::Info,
                        message: format!("mention of '{project}' could be linked"),
                        fix: Some(Fix::AddWikilink {
                            target: project.clone(),
                            context,
                        }),
                    });
                }
            }
        }
    }

    tracing::info!(violation_count = report.violations.len(), "linking lint complete");
    report
}

/// Apply link suggestions: insert [[wikilinks]] at first mention.
#[instrument(skip(notes, config))]
pub fn apply_linking(vault_root: &Path, notes: &[Note], config: &LinkingConfig) -> eyre::Result<usize> {
    let report = lint_linking(notes, config);
    let mut fixed_count = 0;

    // Group fixes by file
    let mut fixes_by_path: std::collections::HashMap<&std::path::Path, Vec<&str>> = std::collections::HashMap::new();
    for violation in &report.violations {
        if let Some(Fix::AddWikilink { target, .. }) = &violation.fix {
            fixes_by_path.entry(violation.path.as_path()).or_default().push(target);
        }
    }

    for (path, targets) in &fixes_by_path {
        let abs_path = vault_root.join(path);
        let content = std::fs::read_to_string(&abs_path)?;
        let mut new_content = content.clone();

        for target in targets {
            // Find the first mention and wrap it in [[]]
            if let Some(new) = insert_first_wikilink(&new_content, target) {
                new_content = new;
            }
        }

        if new_content != content {
            std::fs::write(&abs_path, &new_content)?;
            tracing::info!(path = %path.display(), "inserted wikilinks");
            fixed_count += 1;
        }
    }

    Ok(fixed_count)
}

/// Extract all existing wikilink targets from body (lowercased).
fn extract_existing_links(body: &str) -> HashSet<String> {
    EXISTING_LINK_RE
        .captures_iter(body)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_lowercase()))
        .collect()
}

/// Find a case-insensitive mention of a term in body text.
/// Returns surrounding context if found.
fn find_mention(body: &str, title: &str, stem: &str) -> Option<String> {
    let body_lower = body.to_lowercase();

    // Try title first, then stem
    for term in [title, stem] {
        let term_lower = term.to_lowercase();
        if term_lower.len() < 3 {
            continue;
        }

        if let Some(pos) = body_lower.find(&term_lower) {
            // Verify it's a word boundary (not inside another word)
            let before_char = body[..pos].chars().last().unwrap_or(' ');
            let after_pos = pos + term_lower.len();
            let after_char = body[after_pos..].chars().next().unwrap_or(' ');

            if before_char.is_ascii_alphanumeric() || after_char.is_ascii_alphanumeric() {
                continue;
            }

            // Extract context (surrounding ~20 chars, snapped to char boundaries)
            let start = body.floor_char_boundary(pos.saturating_sub(20));
            let end = body.ceil_char_boundary((pos + term.len() + 20).min(body.len()));
            let context = body[start..end].to_string();
            return Some(context);
        }
    }

    None
}

/// Insert a wikilink at the first mention of a target in content.
fn insert_first_wikilink(content: &str, target: &str) -> Option<String> {
    let pattern = format!(r"(?i)\b{}\b", regex::escape(target));
    let re = Regex::new(&pattern).ok()?;

    // Only replace the first occurrence
    if let Some(mat) = re.find(content) {
        let before = &content[..mat.start()];
        let matched = &content[mat.start()..mat.end()];

        // Don't insert if already inside a wikilink
        if before.ends_with("[[") || content[mat.end()..].starts_with("]]") {
            return None;
        }

        let after = &content[mat.end()..];
        Some(format!("{before}[[{matched}]]{after}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestVault;

    #[test]
    fn test_concept_linking_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.linking;

        let report = lint_linking(&notes, &config);
        // rust-guide.md body mentions "Python Guide" - should suggest linking
        assert!(
            report
                .violations
                .iter()
                .any(|vi| vi.path.to_string_lossy() == "rust-guide.md"
                    && vi.rule == "linking.concept"
                    && vi.message.contains("Python Guide"))
        );
    }

    #[test]
    fn test_person_entity_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.linking;

        let report = lint_linking(&notes, &config);
        // daily-standup.md mentions "John Smith"
        assert!(
            report
                .violations
                .iter()
                .any(|vi| vi.path.to_string_lossy() == "daily-standup.md" && vi.rule == "linking.person")
        );
    }

    #[test]
    fn test_already_linked_not_suggested() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.linking;

        let report = lint_linking(&notes, &config);
        // python-guide.md already has [[rust-guide]] - should NOT suggest it again
        assert!(
            !report
                .violations
                .iter()
                .any(|vi| vi.path.to_string_lossy() == "python-guide.md"
                    && vi.rule == "linking.concept"
                    && vi.message.contains("rust-guide"))
        );
    }

    #[test]
    fn test_insert_first_wikilink() {
        let content = "Working on obsidian-cortex and obsidian-cortex improvements.";
        let result = insert_first_wikilink(content, "obsidian-cortex");
        assert!(result.is_some());
        let result = result.expect("should have result");
        assert!(result.starts_with("Working on [[obsidian-cortex]]"));
        assert_eq!(result.matches("[[").count(), 1);
    }

    #[test]
    fn test_extract_existing_links() {
        let body = "See [[note-a]] and [[note-b|display]].";
        let links = extract_existing_links(body);
        assert!(links.contains("note-a"));
        assert!(links.contains("note-b"));
    }
}
