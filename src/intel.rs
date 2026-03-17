use chrono::{Datelike, Local, NaiveDate};
use eyre::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::instrument;

use crate::cli::IntelOpts;
use crate::config::IntelConfig;
use crate::vault::Note;

/// Generate intelligence outputs (daily digest, weekly review).
#[instrument(skip(notes, config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_intel(vault_root: &Path, notes: &[Note], config: &IntelConfig, opts: &IntelOpts) -> Result<()> {
    if opts.daily || !opts.weekly {
        generate_daily_digest(vault_root, notes, config, opts)?;
    }

    if opts.weekly {
        generate_weekly_review(vault_root, notes, config, opts)?;
    }

    Ok(())
}

/// Generate a daily digest note.
fn generate_daily_digest(vault_root: &Path, notes: &[Note], config: &IntelConfig, opts: &IntelOpts) -> Result<()> {
    let today = Local::now().format("%Y-%m-%d").to_string();
    tracing::info!(date = %today, "generating daily digest");

    // Find notes modified today
    let today_date = Local::now().date_naive();
    let recent_notes: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            n.frontmatter
                .date
                .as_ref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                == Some(today_date)
        })
        .collect();

    // Gather tags from recent notes
    let mut tag_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for note in &recent_notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut top_tags: Vec<(&&str, &usize)> = tag_counts.iter().collect();
    top_tags.sort_by(|a, b| b.1.cmp(a.1));
    let top_tags: Vec<&str> = top_tags.iter().take(5).map(|(t, _)| **t).collect();

    // Generate digest content
    let mut digest = String::new();
    digest.push_str(&format!(
        "---\ntitle: Daily Digest {today}\ndate: {today}\ntype: digest\ntags: [digest]\n---\n\n"
    ));
    digest.push_str(&format!("# Daily Digest - {today}\n\n"));

    digest.push_str("## Notes Today\n\n");
    if recent_notes.is_empty() {
        digest.push_str("No notes created or updated today.\n\n");
    } else {
        for note in &recent_notes {
            let title = note
                .frontmatter
                .title
                .as_deref()
                .unwrap_or_else(|| note.path.to_str().unwrap_or("untitled"));
            let stem = note.path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");
            digest.push_str(&format!("- [[{stem}|{title}]]\n"));
        }
        digest.push('\n');
    }

    if !top_tags.is_empty() {
        digest.push_str("## Active Topics\n\n");
        for tag in &top_tags {
            digest.push_str(&format!("- #{tag}\n"));
        }
        digest.push('\n');
    }

    digest.push_str(&format!(
        "## Stats\n\n- Total vault notes: {}\n- Notes today: {}\n",
        notes.len(),
        recent_notes.len()
    ));

    // Write to output path
    let output_path = resolve_output_path(vault_root, config, opts, &format!("daily-{today}.md"));
    write_intel_output(&output_path, &digest)?;

    println!("Generated daily digest: {}", output_path.display());
    Ok(())
}

/// Generate a weekly review note.
fn generate_weekly_review(vault_root: &Path, notes: &[Note], config: &IntelConfig, opts: &IntelOpts) -> Result<()> {
    let today = Local::now().date_naive();
    let week_start = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let week_str = week_start.format("%Y-%m-%d").to_string();

    tracing::info!(week_start = %week_str, "generating weekly review");

    // Find notes from this week
    let week_notes: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            n.frontmatter
                .date
                .as_ref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .is_some_and(|d| d >= week_start && d <= today)
        })
        .collect();

    // Group by type
    let mut by_type: std::collections::HashMap<&str, Vec<&Note>> = std::collections::HashMap::new();
    for note in &week_notes {
        let note_type = note.frontmatter.note_type.as_deref().unwrap_or("untyped");
        by_type.entry(note_type).or_default().push(note);
    }

    // Gather all tags
    let mut tag_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for note in &week_notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
    }

    let mut top_tags: Vec<(&&str, &usize)> = tag_counts.iter().collect();
    top_tags.sort_by(|a, b| b.1.cmp(a.1));
    let top_tags: Vec<(&str, usize)> = top_tags.iter().take(10).map(|(t, c)| (**t, **c)).collect();

    let today_str = today.format("%Y-%m-%d").to_string();

    // Generate review
    let mut review = String::new();
    review.push_str(&format!(
        "---\ntitle: Weekly Review {week_str}\ndate: {today_str}\ntype: review\ntags: [review]\n---\n\n"
    ));
    review.push_str(&format!("# Weekly Review - Week of {week_str}\n\n"));

    review.push_str(&format!(
        "## Summary\n\n- Notes this week: {}\n- Total vault size: {}\n\n",
        week_notes.len(),
        notes.len()
    ));

    if !by_type.is_empty() {
        review.push_str("## By Type\n\n");
        let mut types: Vec<_> = by_type.iter().collect();
        types.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        for (note_type, type_notes) in types {
            review.push_str(&format!("### {note_type} ({})\n\n", type_notes.len()));
            for note in type_notes {
                let title = note
                    .frontmatter
                    .title
                    .as_deref()
                    .unwrap_or_else(|| note.path.to_str().unwrap_or("untitled"));
                let stem = note.path.file_stem().and_then(|s| s.to_str()).unwrap_or("untitled");
                review.push_str(&format!("- [[{stem}|{title}]]\n"));
            }
            review.push('\n');
        }
    }

    if !top_tags.is_empty() {
        review.push_str("## Top Topics\n\n");
        for (tag, count) in &top_tags {
            review.push_str(&format!("- #{tag} ({count} notes)\n"));
        }
        review.push('\n');
    }

    let output_path = resolve_output_path(vault_root, config, opts, &format!("weekly-{week_str}.md"));
    write_intel_output(&output_path, &review)?;

    println!("Generated weekly review: {}", output_path.display());
    Ok(())
}

/// Resolve the output path for an intel file.
fn resolve_output_path(vault_root: &Path, config: &IntelConfig, opts: &IntelOpts, filename: &str) -> PathBuf {
    if let Some(ref output) = opts.output {
        return output.clone();
    }
    vault_root.join(&config.output_path).join(filename)
}

/// Write intel output to disk.
fn write_intel_output(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context(format!("failed to create directory {}", parent.display()))?;
    }
    std::fs::write(path, content).context(format!("failed to write {}", path.display()))?;
    tracing::info!(path = %path.display(), "wrote intel output");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IntelConfig;
    use crate::vault::Frontmatter;

    fn make_note(path: &str, title: &str, date: &str, note_type: &str, tags: Vec<&str>) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter {
                title: Some(title.to_string()),
                date: Some(date.to_string()),
                note_type: Some(note_type.to_string()),
                tags: Some(tags.into_iter().map(String::from).collect()),
                extra: Default::default(),
            },
            body: String::new(),
            raw: String::new(),
        }
    }

    #[test]
    fn test_daily_digest_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let today = Local::now().format("%Y-%m-%d").to_string();

        let notes = vec![
            make_note("note-a.md", "Note A", &today, "note", vec!["rust"]),
            make_note("note-b.md", "Note B", "2020-01-01", "note", vec!["python"]),
        ];

        let config = IntelConfig {
            daily_note: true,
            weekly_review: false,
            fabric_patterns: vec![],
            output_path: "output".to_string(),
        };

        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: None,
        };

        run_intel(tmp.path(), &notes, &config, &opts).expect("run_intel");

        let digest_path = tmp.path().join("output").join(format!("daily-{today}.md"));
        assert!(digest_path.exists());
        let content = std::fs::read_to_string(&digest_path).expect("read digest");
        assert!(content.contains("Daily Digest"));
        assert!(content.contains("Note A"));
    }

    #[test]
    fn test_weekly_review_generation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let today = Local::now().format("%Y-%m-%d").to_string();

        let notes = vec![make_note(
            "note-a.md",
            "Note A",
            &today,
            "video",
            vec!["rust", "ai-llm"],
        )];

        let config = IntelConfig {
            daily_note: false,
            weekly_review: true,
            fabric_patterns: vec![],
            output_path: "output".to_string(),
        };

        let opts = IntelOpts {
            daily: false,
            weekly: true,
            output: None,
        };

        run_intel(tmp.path(), &notes, &config, &opts).expect("run_intel");

        // Find the generated file
        let output_dir = tmp.path().join("output");
        assert!(output_dir.exists());
        let files: Vec<_> = std::fs::read_dir(&output_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .collect();
        assert!(!files.is_empty());
    }

    #[test]
    fn test_resolve_output_path_with_explicit_output() {
        let config = IntelConfig::default();
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: Some(PathBuf::from("/custom/path.md")),
        };

        let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily.md");
        assert_eq!(path, PathBuf::from("/custom/path.md"));
    }

    #[test]
    fn test_resolve_output_path_default() {
        let config = IntelConfig {
            output_path: "ai-output".to_string(),
            ..Default::default()
        };
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: None,
        };

        let path = resolve_output_path(Path::new("/vault"), &config, &opts, "daily-2026-03-16.md");
        assert_eq!(path, PathBuf::from("/vault/ai-output/daily-2026-03-16.md"));
    }
}
