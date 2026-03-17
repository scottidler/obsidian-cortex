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
    use crate::vault::Frontmatter;
    use std::path::PathBuf;

    fn make_note(path: &str, body: &str) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter::default(),
            body: body.to_string(),
            raw: String::new(),
        }
    }

    fn default_config() -> BrokenLinksConfig {
        BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        }
    }

    #[test]
    fn test_extract_wikilinks() {
        let body = "See [[note-a]] and [[note-b|display]]. Also [[folder/note-c]].";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["note-a", "note-b", "folder/note-c"]);
    }

    #[test]
    fn test_no_broken_links() {
        let notes = vec![
            make_note("note-a.md", "Links to [[note-b]]."),
            make_note("note-b.md", "Links to [[note-a]]."),
        ];

        let report = lint_broken_links(&notes, &default_config());
        assert!(report.is_empty());
    }

    #[test]
    fn test_broken_link_detected() {
        let notes = vec![make_note("note-a.md", "Links to [[nonexistent]].")];

        let report = lint_broken_links(&notes, &default_config());
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule, "broken-links.wikilink");
        assert!(report.violations[0].message.contains("nonexistent"));
    }

    #[test]
    fn test_case_insensitive_matching() {
        let notes = vec![
            make_note("My-Note.md", ""),
            make_note("linker.md", "Links to [[my-note]]."),
        ];

        let report = lint_broken_links(&notes, &default_config());
        assert!(report.is_empty());
    }

    #[test]
    fn test_disabled_check() {
        let notes = vec![make_note("a.md", "Links to [[nonexistent]].")];
        let config = BrokenLinksConfig {
            check_wikilinks: false,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &config);
        assert!(report.is_empty());
    }

    #[test]
    fn test_link_with_display_text() {
        let notes = vec![
            make_note("target.md", ""),
            make_note("source.md", "See [[target|some display text]]."),
        ];

        let report = lint_broken_links(&notes, &default_config());
        assert!(report.is_empty());
    }

    #[test]
    fn test_no_wikilinks_in_body() {
        let notes = vec![make_note("solo.md", "No links here, just text.")];

        let report = lint_broken_links(&notes, &default_config());
        assert!(report.is_empty());
    }
}
