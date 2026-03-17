use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use tracing::instrument;

use crate::config::BrokenLinksConfig;
use crate::report::{Report, Severity, Violation};
use crate::vault::Note;

/// Regex to match [[wikilinks]] and [[wikilinks|display text]].
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]").expect("valid wikilink regex"));

/// Run broken link detection on all notes.
#[instrument(skip(notes, config))]
pub fn lint_broken_links(notes: &[Note], config: &BrokenLinksConfig) -> Report {
    let mut report = Report::default();

    if !config.check_wikilinks {
        return report;
    }

    // Build a set of all note stems (case-insensitive for Obsidian compatibility)
    let note_stems: HashSet<String> = notes
        .iter()
        .filter_map(|n| n.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()))
        .collect();

    // Also include stems with path for disambiguated links
    let note_paths: HashSet<String> = notes
        .iter()
        .map(|n| {
            let path = n.path.with_extension("");
            path.to_string_lossy().to_lowercase()
        })
        .collect();

    for note in notes {
        let links = extract_wikilinks(&note.body);

        for link in links {
            let target_lower = link.to_lowercase();

            // Check if target exists (stem match or path match)
            let exists = note_stems.contains(&target_lower) || note_paths.contains(&target_lower.replace('\\', "/"));

            if !exists {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "broken-links.wikilink".to_string(),
                    severity: Severity::Error,
                    message: format!("broken wikilink: [[{link}]]"),
                    fix: None,
                });
            }
        }
    }

    tracing::info!(violation_count = report.violations.len(), "broken links lint complete");
    report
}

/// Extract all wikilink targets from a note body.
fn extract_wikilinks(body: &str) -> Vec<String> {
    WIKILINK_RE
        .captures_iter(body)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().trim().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestVault;

    #[test]
    fn test_extract_wikilinks() {
        let body = "See [[note-a]] and [[note-b|display]]. Also [[folder/note-c]].";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["note-a", "note-b", "folder/note-c"]);
    }

    #[test]
    fn test_broken_link_detected_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.broken_links;

        let report = lint_broken_links(&notes, &config);
        // linker.md has [[nonexistent-page]] which is broken
        assert!(
            report
                .violations
                .iter()
                .any(|vi| vi.path.to_string_lossy() == "linker.md" && vi.message.contains("nonexistent-page"))
        );
    }

    #[test]
    fn test_valid_links_not_flagged() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.broken_links;

        let report = lint_broken_links(&notes, &config);
        // python-guide.md links to [[rust-guide]] which exists - should NOT be broken
        assert!(
            !report
                .violations
                .iter()
                .any(|vi| vi.path.to_string_lossy() == "python-guide.md" && vi.message.contains("rust-guide"))
        );
    }

    #[test]
    fn test_disabled_check() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = BrokenLinksConfig {
            check_wikilinks: false,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &config);
        assert!(report.is_empty());
    }
}
