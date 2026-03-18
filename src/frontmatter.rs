use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use tracing::instrument;

use crate::config::FrontmatterConfig;
use crate::naming::to_slug;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid date regex"));

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("valid tag regex"));

/// Run frontmatter validation on all notes.
#[instrument(skip(notes, config))]
pub fn lint_frontmatter(notes: &[Note], config: &FrontmatterConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        validate_note(note, config, &mut report);
    }

    tracing::info!(violation_count = report.violations.len(), "frontmatter lint complete");
    report
}

fn validate_note(note: &Note, config: &FrontmatterConfig, report: &mut Report) {
    let fm = &note.frontmatter;

    // Check if frontmatter exists at all
    if fm.is_empty() && !note.raw.trim_start().starts_with("---") {
        report.add(Violation {
            path: note.path.clone(),
            rule: "frontmatter.missing".to_string(),
            severity: Severity::Error,
            message: "file has no frontmatter".to_string(),
            fix: Some(Fix::SetFrontmatter {
                key: "__insert_block__".to_string(),
                value: serde_yaml::Value::Null,
            }),
        });
        return;
    }

    // Check required fields
    for field in &config.required {
        let present = match field.as_str() {
            "title" => fm.title.is_some(),
            "date" => fm.date.is_some(),
            "type" => fm.note_type.is_some(),
            "domain" => fm.domain.is_some(),
            "origin" => fm.origin.is_some(),
            "status" => fm.status.is_some(),
            "tags" => fm.tags.is_some(),
            "source" => fm.source.is_some(),
            "creator" => fm.creator.is_some(),
            _ => fm.extra.contains_key(field),
        };

        if !present {
            let fix = if field == "title" && config.auto_title {
                let title = title_from_filename(&note.path);
                Some(Fix::SetFrontmatter {
                    key: "title".to_string(),
                    value: serde_yaml::Value::String(title),
                })
            } else {
                None
            };

            report.add(Violation {
                path: note.path.clone(),
                rule: format!("frontmatter.required.{field}"),
                severity: Severity::Error,
                message: format!("missing required field: {field}"),
                fix,
            });
        }
    }

    // Validate date format
    if let Some(ref date) = fm.date
        && !DATE_RE.is_match(date)
    {
        report.add(Violation {
            path: note.path.clone(),
            rule: "frontmatter.date-format".to_string(),
            severity: Severity::Warning,
            message: format!("date '{date}' is not YYYY-MM-DD format"),
            fix: None,
        });
    }

    // Validate tag format
    if let Some(ref tags) = fm.tags {
        for tag in tags {
            if !TAG_RE.is_match(tag) {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: "frontmatter.tag-format".to_string(),
                    severity: Severity::Warning,
                    message: format!("tag '{tag}' is not lowercase-hyphenated"),
                    fix: None,
                });
            }
        }
    }

    // Check type-specific required fields
    if let Some(ref note_type) = fm.note_type
        && let Some(type_fields) = config.type_fields.get(note_type)
    {
        for field in type_fields {
            let present = match field.as_str() {
                "source" => fm.source.is_some(),
                "creator" => fm.creator.is_some(),
                "domain" => fm.domain.is_some(),
                "origin" => fm.origin.is_some(),
                "status" => fm.status.is_some(),
                _ => fm.extra.contains_key(field),
            };
            if !present {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: format!("frontmatter.type-field.{note_type}.{field}"),
                    severity: Severity::Warning,
                    message: format!("type '{note_type}' missing field: {field}"),
                    fix: None,
                });
            }
        }
    }
}

/// Derive a title from a filename.
fn title_from_filename(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");

    // If it's already a slug, convert to title case
    stem.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let upper: String = first.to_uppercase().collect();
                    format!("{upper}{}", chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Apply frontmatter fixes to notes.
#[instrument(skip(notes, config))]
pub fn apply_frontmatter(vault_root: &Path, notes: &[Note], config: &FrontmatterConfig) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let mut report = Report::default();
        validate_note(note, config, &mut report);

        if report.is_empty() {
            continue;
        }

        let has_insert_block = report
            .violations
            .iter()
            .any(|v| matches!(&v.fix, Some(Fix::SetFrontmatter { key, .. }) if key == "__insert_block__"));

        if has_insert_block {
            // Insert minimal frontmatter block
            let title = title_from_filename(&note.path);
            let slug = to_slug(note.path.file_name().and_then(|f| f.to_str()).unwrap_or("untitled.md"));
            let date = chrono::Local::now().format("%Y-%m-%d").to_string();
            let new_fm = format!("---\ntitle: {title}\ndate: {date}\ntype: note\ntags: []\n---\n");

            let abs_path = vault_root.join(&note.path);
            let new_content = format!("{new_fm}{}", note.raw);
            std::fs::write(&abs_path, new_content)?;
            tracing::info!(path = %note.path.display(), slug = %slug, "inserted frontmatter block");
            fixed_count += 1;
        } else {
            // Apply individual field fixes via targeted string replacement
            let abs_path = vault_root.join(&note.path);
            let mut content = note.raw.clone();
            let mut modified = false;

            for violation in &report.violations {
                if let Some(Fix::SetFrontmatter { key, value }) = &violation.fix
                    && key == "title"
                    && let serde_yaml::Value::String(title) = value
                    && let Some(pos) = content.find("---\n")
                {
                    let insert_pos = pos + 4;
                    content.insert_str(insert_pos, &format!("title: {title}\n"));
                    modified = true;
                }
            }

            if modified {
                std::fs::write(&abs_path, &content)?;
                tracing::info!(path = %note.path.display(), "applied frontmatter fixes");
                fixed_count += 1;
            }
        }
    }

    Ok(fixed_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestVault;

    #[test]
    fn test_valid_notes_pass() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;

        let report = lint_frontmatter(&notes, &config);
        // rust-guide.md has all required fields - should NOT be flagged
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "rust-guide.md")
        );
    }

    #[test]
    fn test_missing_frontmatter_detected() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;

        let report = lint_frontmatter(&notes, &config);
        // bare-note.md has no frontmatter
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "bare-note.md" && v.rule == "frontmatter.missing")
        );
    }

    #[test]
    fn test_missing_required_fields() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;

        let report = lint_frontmatter(&notes, &config);
        // partial-frontmatter.md has title but missing date, type, tags
        let partial_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.path.to_string_lossy() == "partial-frontmatter.md")
            .collect();
        assert_eq!(partial_violations.len(), 3);
    }

    #[test]
    fn test_type_specific_fields() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;

        let report = lint_frontmatter(&notes, &config);
        // cool-video.md is type=video but missing source, channel
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "cool-video.md" && v.rule.contains("type-field.video"))
        );
    }

    #[test]
    fn test_title_from_filename() {
        assert_eq!(title_from_filename(Path::new("hello-world.md")), "Hello World");
        assert_eq!(title_from_filename(Path::new("my-note-123.md")), "My Note 123");
    }

    #[test]
    fn test_apply_inserts_frontmatter() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;

        let count = apply_frontmatter(v.root(), &notes, &config).expect("apply");
        assert!(count > 0);

        // bare-note.md should now have frontmatter
        let content = v.read("bare-note.md");
        assert!(content.starts_with("---\n"));
        assert!(content.contains("title:"));
    }
}
