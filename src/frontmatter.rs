use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use tracing::instrument;

use crate::config::{FrontmatterConfig, SchemaConfig};
use crate::naming::to_slug;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

static DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid date regex"));

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("valid tag regex"));

/// Deprecated fields that should be renamed to their v2 equivalents.
const DEPRECATED_RENAMES: &[(&str, &str)] = &[
    ("url", "source"),
    ("author", "creator"),
    ("uploader", "creator"),
    ("channel", "creator"),
    ("duration_min", "duration"),
    ("trace_id", "trace"),
    ("folder", "domain"),
];

/// Deprecated fields that should be dropped entirely.
const DEPRECATED_DROPS: &[&str] = &["day", "time", "ww", "ref"];

/// Run frontmatter validation on all notes.
#[instrument(skip(notes, config, schema))]
pub fn lint_frontmatter(notes: &[Note], config: &FrontmatterConfig, schema: &SchemaConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        validate_note(note, config, schema, &mut report);
    }

    tracing::info!(violation_count = report.violations.len(), "frontmatter lint complete");
    report
}

fn validate_note(note: &Note, config: &FrontmatterConfig, schema: &SchemaConfig, report: &mut Report) {
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

    // Check required fields (with exemptions)
    for field in &config.required {
        if !is_field_required(field, note, config) {
            continue;
        }

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

    // Validate enum fields against schema
    validate_enum("type", fm.note_type.as_deref(), &schema.types, note, report);
    validate_enum("domain", fm.domain.as_deref(), &schema.domains, note, report);
    validate_enum("origin", fm.origin.as_deref(), &schema.origins, note, report);
    validate_enum("status", fm.status.as_deref(), &schema.statuses, note, report);
    // method lives in extra
    if let Some(serde_yaml::Value::String(method)) = fm.extra.get("method") {
        validate_enum_value("method", method, &schema.methods, note, report);
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

    // Detect deprecated fields
    for (old_name, new_name) in DEPRECATED_RENAMES {
        if fm.extra.contains_key(*old_name) {
            report.add(Violation {
                path: note.path.clone(),
                rule: format!("frontmatter.deprecated.{old_name}"),
                severity: Severity::Warning,
                message: format!("deprecated field '{old_name}' should be renamed to '{new_name}'"),
                fix: None,
            });
        }
    }
    for drop_name in DEPRECATED_DROPS {
        if fm.extra.contains_key(*drop_name) {
            report.add(Violation {
                path: note.path.clone(),
                rule: format!("frontmatter.deprecated.{drop_name}"),
                severity: Severity::Warning,
                message: format!("deprecated field '{drop_name}' should be removed"),
                fix: None,
            });
        }
    }
}

/// Check whether a required field is actually required for this note,
/// considering type-based and path-based exemptions.
fn is_field_required(field: &str, note: &Note, config: &FrontmatterConfig) -> bool {
    // Check type-based exemptions
    if let Some(ref note_type) = note.frontmatter.note_type
        && let Some(exempt_fields) = config.exempt.get(note_type)
        && exempt_fields.iter().any(|f| f == field)
    {
        return false;
    }

    // Check path-based exemptions
    for (pattern, exempt_fields) in &config.path_exempt {
        if let Ok(glob) = glob::Pattern::new(pattern)
            && glob.matches_path(&note.path)
            && exempt_fields.iter().any(|f| f == field)
        {
            return false;
        }
    }

    true
}

/// Validate an enum field value against allowed values.
/// Only reports if the field is present and the allowed list is non-empty.
fn validate_enum(field_name: &str, value: Option<&str>, allowed: &[String], note: &Note, report: &mut Report) {
    if let Some(value) = value {
        validate_enum_value(field_name, value, allowed, note, report);
    }
}

fn validate_enum_value(field_name: &str, value: &str, allowed: &[String], note: &Note, report: &mut Report) {
    if !allowed.is_empty() && !allowed.iter().any(|v| v == value) {
        report.add(Violation {
            path: note.path.clone(),
            rule: format!("frontmatter.enum.{field_name}"),
            severity: Severity::Error,
            message: format!("{field_name} '{value}' is not valid; allowed: [{}]", allowed.join(", ")),
            fix: None,
        });
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
#[instrument(skip(notes, config, schema))]
pub fn apply_frontmatter(
    vault_root: &Path,
    notes: &[Note],
    config: &FrontmatterConfig,
    schema: &SchemaConfig,
) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let mut report = Report::default();
        validate_note(note, config, schema, &mut report);

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
    use crate::config::SchemaConfig;
    use crate::testutil::TestVault;

    fn test_schema() -> SchemaConfig {
        SchemaConfig {
            domains: vec![
                "ai",
                "tech",
                "football",
                "work",
                "writing",
                "music",
                "spanish",
                "knowledge",
                "resources",
                "system",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            types: vec![
                "youtube", "article", "github", "social", "book", "video", "research", "daily", "meeting", "note",
                "vocab", "moc", "link", "poem", "system",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            origins: vec!["authored", "assisted", "generated"]
                .into_iter()
                .map(String::from)
                .collect(),
            statuses: vec!["unread", "reading", "reviewed", "starred"]
                .into_iter()
                .map(String::from)
                .collect(),
            methods: vec!["http", "telegram", "clipboard", "cli", "manual"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }

    #[test]
    fn test_valid_notes_pass() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // rust-guide.md has all required fields - should NOT be flagged for required
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "rust-guide.md" && v.rule.starts_with("frontmatter.required"))
        );
    }

    #[test]
    fn test_missing_frontmatter_detected() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
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
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // partial-frontmatter.md has title but missing date, type, tags
        let partial_violations: Vec<_> = report
            .violations
            .iter()
            .filter(|v| {
                v.path.to_string_lossy() == "partial-frontmatter.md" && v.rule.starts_with("frontmatter.required")
            })
            .collect();
        assert_eq!(partial_violations.len(), 3);
    }

    #[test]
    fn test_type_specific_fields() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // cool-video.md is type=video but missing source, creator
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
        let schema = test_schema();

        let count = apply_frontmatter(v.root(), &notes, &config, &schema).expect("apply");
        assert!(count > 0);

        // bare-note.md should now have frontmatter
        let content = v.read("bare-note.md");
        assert!(content.starts_with("---\n"));
        assert!(content.contains("title:"));
    }

    #[test]
    fn test_enum_validation_invalid_domain() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // bad-enums.md has domain: tech-stuff which is invalid
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.domain")
        );
    }

    #[test]
    fn test_enum_validation_invalid_type() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // bad-enums.md has type: blogpost which is invalid
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.type")
        );
    }

    #[test]
    fn test_enum_validation_invalid_origin() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // bad-enums.md has origin: robot which is invalid
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy() == "bad-enums.md" && v.rule == "frontmatter.enum.origin")
        );
    }

    #[test]
    fn test_enum_validation_skipped_when_schema_empty() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let empty_schema = SchemaConfig::default();

        let report = lint_frontmatter(&notes, &config, &empty_schema);
        // With empty schema, no enum violations should appear
        assert!(!report.violations.iter().any(|v| v.rule.starts_with("frontmatter.enum")));
    }

    #[test]
    fn test_daily_note_exempt_from_domain() {
        let v = TestVault::new();
        let notes = v.scan();
        let mut config = v.config().actions.frontmatter;
        config.required = vec![
            "title".to_string(),
            "date".to_string(),
            "type".to_string(),
            "domain".to_string(),
            "origin".to_string(),
            "tags".to_string(),
        ];
        config.exempt.insert("daily".to_string(), vec!["domain".to_string()]);
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // daily/2026-03-18.md is type: daily, has no domain, should NOT be flagged
        assert!(
            !report
                .violations
                .iter()
                .any(|v| v.path.to_string_lossy().contains("2026-03-18") && v.rule == "frontmatter.required.domain")
        );
    }

    #[test]
    fn test_inbox_note_exempt_from_domain() {
        let v = TestVault::new();
        let notes = v.scan();
        let mut config = v.config().actions.frontmatter;
        config.required = vec![
            "title".to_string(),
            "date".to_string(),
            "type".to_string(),
            "domain".to_string(),
            "origin".to_string(),
            "tags".to_string(),
        ];
        config
            .path_exempt
            .insert("inbox/**".to_string(), vec!["domain".to_string()]);
        let schema = test_schema();

        let report = lint_frontmatter(&notes, &config, &schema);
        // inbox/untriaged-link.md has no domain, should NOT be flagged
        assert!(
            !report.violations.iter().any(|v| v.path.to_string_lossy().contains("untriaged-link")
                && v.rule == "frontmatter.required.domain")
        );
    }

    #[test]
    fn test_deprecated_field_detection() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.frontmatter;
        let schema = SchemaConfig::default();

        let report = lint_frontmatter(&notes, &config, &schema);
        // legacy-note.md has url, author, duration_min, folder
        let legacy_deprecated: Vec<_> = report
            .violations
            .iter()
            .filter(|v| v.path.to_string_lossy() == "legacy-note.md" && v.rule.starts_with("frontmatter.deprecated"))
            .collect();
        assert!(
            legacy_deprecated.len() >= 4,
            "expected at least 4 deprecated field violations, got {}",
            legacy_deprecated.len()
        );
    }
}
