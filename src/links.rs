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

/// Run broken link detection.
/// `lintable_notes` are checked for violations; `all_notes` are used to build
/// the resolution indexes (so excluded files still count as valid link targets).
#[instrument(skip(lintable_notes, all_notes, config))]
pub fn lint_broken_links(lintable_notes: &[Note], all_notes: &[Note], config: &BrokenLinksConfig) -> Report {
    let mut report = Report::default();

    if !config.check_wikilinks {
        return report;
    }

    // Build indexes from ALL notes (including excluded) so that links to
    // excluded files still resolve correctly.

    // Stem index: file stems in lowercase
    let note_stems: HashSet<String> = all_notes
        .iter()
        .filter_map(|n| n.path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_lowercase()))
        .collect();

    // Path index: full paths with extension stripped, lowercased
    let note_paths: HashSet<String> = all_notes
        .iter()
        .map(|n| {
            let path = n.path.with_extension("");
            path.to_string_lossy().to_lowercase()
        })
        .collect();

    // Title index: lowercased frontmatter titles for exact title match
    let title_set: HashSet<String> = all_notes
        .iter()
        .filter_map(|n| n.frontmatter.title.as_ref())
        .map(|t| t.to_lowercase())
        .collect();

    // Only check lintable notes for violations
    for note in lintable_notes {
        let links = extract_wikilinks(&note.body);

        for link in links {
            let target_lower = link.to_lowercase();
            let target_slug = crate::naming::to_slug(&link);

            // Resolution order: stem -> path -> title -> slug-of-target
            let exists = note_stems.contains(&target_lower)
                || note_paths.contains(&target_lower.replace('\\', "/"))
                || title_set.contains(&target_lower)
                || note_stems.contains(&target_slug);

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

        let report = lint_broken_links(&notes, &notes, &config);
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

        let report = lint_broken_links(&notes, &notes, &config);
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

        let report = lint_broken_links(&notes, &notes, &config);
        assert!(report.is_empty());
    }

    #[test]
    fn test_title_case_wikilink_resolves_via_slug() {
        let v = TestVault::new();
        v.add_note(
            "zone-blocking-families.md",
            "---\ntitle: Zone Blocking Families\ndate: 2026-03-18\ntype: note\ndomain: football\norigin: authored\ntags:\n  - football\n---\nZone blocking content.\n",
        );
        v.add_note(
            "borg-ledger.md",
            "---\ntitle: Borg Ledger\ndate: 2026-03-18\ntype: system\ndomain: system\norigin: generated\ntags: []\n---\nSee [[Zone Blocking Families]] for details.\n",
        );
        let notes = v.scan();
        let config = v.config().actions.broken_links;

        let report = lint_broken_links(&notes, &notes, &config);
        // [[Zone Blocking Families]] should resolve to zone-blocking-families.md via slug match
        assert!(
            !report
                .violations
                .iter()
                .any(|vi| vi.message.contains("Zone Blocking Families")),
            "title-case wikilink should resolve via slug match"
        );
    }

    #[test]
    fn test_title_match_wikilink_resolves() {
        let v = TestVault::new();
        // Note where title differs from filename
        v.add_note(
            "my-custom-slug.md",
            "---\ntitle: A Totally Different Title\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: authored\ntags: []\n---\nContent.\n",
        );
        v.add_note(
            "referrer.md",
            "---\ntitle: Referrer\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: authored\ntags: []\n---\nSee [[A Totally Different Title]] here.\n",
        );
        let notes = v.scan();
        let config = v.config().actions.broken_links;

        let report = lint_broken_links(&notes, &notes, &config);
        assert!(
            !report
                .violations
                .iter()
                .any(|vi| vi.message.contains("A Totally Different Title")),
            "wikilink matching exact title should resolve"
        );
    }

    #[test]
    fn test_excluded_files_still_resolve_as_targets() {
        use crate::testutil::NoteBuilder;

        let all_notes = vec![
            NoteBuilder::new("readme.md")
                .title("README")
                .body("Repo readme.")
                .build(),
            NoteBuilder::new("linker.md")
                .title("Linker")
                .body("See [[readme]] for info.")
                .build(),
        ];
        // Only linker.md is lintable, but readme.md is in all_notes for index
        let lintable_notes = vec![all_notes[1].clone()];
        let config = BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        };

        let report = lint_broken_links(&lintable_notes, &all_notes, &config);
        assert!(
            !report.violations.iter().any(|vi| vi.message.contains("readme")),
            "excluded file should still be a valid link target"
        );
    }
}
