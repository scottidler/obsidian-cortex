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

/// Asset file extensions that indicate a genuinely broken embed/reference
/// when the target file is missing (as opposed to an aspirational note link).
const ASSET_EXTENSIONS: &[&str] = &[
    // Images
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".svg",
    ".webp",
    ".bmp",
    ".tiff",
    // Documents
    ".pdf",
    // Media
    ".mp4",
    ".mp3",
    ".wav",
    ".webm",
    ".ogg",
    ".m4a",
    // Other Obsidian embed types
    ".csv",
    ".excalidraw",
];

/// Check if a wikilink target refers to an asset (image, PDF, media, etc.)
/// based on its file extension.
fn is_asset_reference(target: &str) -> bool {
    let lower = target.to_lowercase();
    ASSET_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Strip fenced code blocks from markdown body so that wikilinks inside
/// code blocks are not extracted. Uses line-by-line state tracking.
fn strip_fenced_code_blocks(body: &str) -> String {
    let mut result = String::with_capacity(body.len());
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            result.push('\n');
            continue;
        }
        if in_fence {
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

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
        let clean_body = strip_fenced_code_blocks(&note.body);
        let links = extract_wikilinks(&clean_body);

        for link in links {
            let target_lower = link.to_lowercase();
            let target_slug = crate::naming::to_slug(&link);

            // Resolution order: stem -> path -> title -> slug-of-target
            let exists = note_stems.contains(&target_lower)
                || note_paths.contains(&target_lower.replace('\\', "/"))
                || title_set.contains(&target_lower)
                || note_stems.contains(&target_slug);

            if !exists {
                // Classify unresolved links by type
                let (rule, severity) = if is_asset_reference(&link) {
                    ("broken-links.asset", Severity::Error)
                } else if link.ends_with('/') {
                    ("broken-links.folder", Severity::Error)
                } else {
                    ("broken-links.unresolved", Severity::Info)
                };

                report.add(Violation {
                    path: note.path.clone(),
                    rule: rule.to_string(),
                    severity,
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
        // linker.md has [[nonexistent-page]] which is broken (unresolved note link)
        let violation = report
            .violations
            .iter()
            .find(|vi| vi.path.to_string_lossy() == "linker.md" && vi.message.contains("nonexistent-page"));
        assert!(violation.is_some(), "should detect broken wikilink");
        assert_eq!(violation.unwrap().rule, "broken-links.unresolved");
        assert_eq!(violation.unwrap().severity, Severity::Info);
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

    #[test]
    fn test_strip_fenced_code_blocks() {
        let body = "Before code\n```\n[[inside-code]]\n```\nAfter code [[outside-code]]";
        let stripped = strip_fenced_code_blocks(body);
        assert!(!stripped.contains("inside-code"));
        assert!(stripped.contains("outside-code"));
    }

    #[test]
    fn test_strip_fenced_code_blocks_with_language() {
        let body = "Text\n```rust\nlet x = \"[[in-rust-block]]\";\n```\n[[real-link]]";
        let stripped = strip_fenced_code_blocks(body);
        assert!(!stripped.contains("in-rust-block"));
        assert!(stripped.contains("real-link"));
    }

    #[test]
    fn test_strip_fenced_code_blocks_preserves_no_fence() {
        let body = "No code blocks here. See [[some-link]].";
        let stripped = strip_fenced_code_blocks(body);
        assert!(stripped.contains("some-link"));
    }

    #[test]
    fn test_is_asset_reference() {
        assert!(is_asset_reference("pasted-image-20240617.png"));
        assert!(is_asset_reference("document.pdf"));
        assert!(is_asset_reference("assets/photo.jpg"));
        assert!(is_asset_reference("recording.mp4"));
        assert!(is_asset_reference("drawing.excalidraw"));
        assert!(is_asset_reference("IMAGE.PNG")); // case-insensitive

        assert!(!is_asset_reference("tmux"));
        assert!(!is_asset_reference("ThePrimeagen"));
        assert!(!is_asset_reference("some-note"));
        assert!(!is_asset_reference("note.md")); // .md is NOT an asset
    }

    #[test]
    fn test_severity_asset_is_error() {
        use crate::testutil::NoteBuilder;

        let notes = vec![
            NoteBuilder::new("test.md")
                .title("Test")
                .body("See ![[missing-image.png]] here.")
                .build(),
        ];
        let config = BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &notes, &config);
        let violation = report
            .violations
            .iter()
            .find(|vi| vi.message.contains("missing-image.png"));
        assert!(violation.is_some(), "should detect missing asset");
        assert_eq!(violation.unwrap().rule, "broken-links.asset");
        assert_eq!(violation.unwrap().severity, Severity::Error);
    }

    #[test]
    fn test_severity_folder_is_error() {
        use crate::testutil::NoteBuilder;

        let notes = vec![
            NoteBuilder::new("test.md")
                .title("Test")
                .body("See [[Old Folder/]] here.")
                .build(),
        ];
        let config = BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &notes, &config);
        let violation = report.violations.iter().find(|vi| vi.message.contains("Old Folder/"));
        assert!(violation.is_some(), "should detect stale folder link");
        assert_eq!(violation.unwrap().rule, "broken-links.folder");
        assert_eq!(violation.unwrap().severity, Severity::Error);
    }

    #[test]
    fn test_severity_unresolved_note_is_info() {
        use crate::testutil::NoteBuilder;

        let notes = vec![
            NoteBuilder::new("test.md")
                .title("Test")
                .body("See [[tmux]] and [[ThePrimeagen]] here.")
                .build(),
        ];
        let config = BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &notes, &config);
        for vi in &report.violations {
            assert_eq!(vi.rule, "broken-links.unresolved");
            assert_eq!(vi.severity, Severity::Info);
        }
        assert_eq!(report.violations.len(), 2);
    }

    #[test]
    fn test_code_block_wikilinks_skipped() {
        use crate::testutil::NoteBuilder;

        let notes = vec![
            NoteBuilder::new("test.md")
                .title("Test")
                .body("Real [[nonexistent-note]] link.\n```\n[[inside-code-block]]\n```\nDone.")
                .build(),
        ];
        let config = BrokenLinksConfig {
            check_wikilinks: true,
            check_urls: false,
        };

        let report = lint_broken_links(&notes, &notes, &config);
        assert!(
            !report
                .violations
                .iter()
                .any(|vi| vi.message.contains("inside-code-block")),
            "wikilinks inside code blocks should be skipped"
        );
        assert!(
            report
                .violations
                .iter()
                .any(|vi| vi.message.contains("nonexistent-note")),
            "wikilinks outside code blocks should be detected"
        );
    }
}
