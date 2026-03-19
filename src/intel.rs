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

/// Process new/unread notes with Fabric pattern.
/// Sets cortex-insights in frontmatter and updates status to processed.
#[instrument(skip(notes, config))]
pub fn process_new_notes(vault_root: &Path, notes: &[Note], config: &IntelConfig) -> Result<usize> {
    let pattern = match &config.on_new_note {
        Some(p) => p.clone(),
        None => return Ok(0),
    };

    if !crate::fabric::is_available() {
        tracing::debug!("fabric not available, skipping new note processing");
        return Ok(0);
    }

    let mut processed = 0;

    for note in notes {
        // Only process unread notes
        if note.frontmatter.status.as_deref() != Some("unread") {
            continue;
        }

        // Skip if already processed
        if note.frontmatter.extra.contains_key("cortex-insights") {
            continue;
        }

        // Skip empty bodies
        if note.body.trim().is_empty() {
            continue;
        }

        let input = crate::fabric::truncate_input(&note.body, config.max_input_tokens);
        match crate::fabric::run_pattern(&pattern, input, config.fabric_timeout_secs) {
            Ok(insights) => {
                let abs_path = vault_root.join(&note.path);
                let content = std::fs::read_to_string(&abs_path)?;

                // Write cortex-insights and update status
                let fields = vec![
                    (
                        "cortex-insights".to_string(),
                        serde_yaml::Value::String(insights.trim().to_string()),
                    ),
                    ("status".to_string(), serde_yaml::Value::String("processed".to_string())),
                ];

                if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &fields) {
                    std::fs::write(&abs_path, new_content)?;
                    tracing::info!(path = %note.path.display(), "processed new note with fabric");
                    processed += 1;
                }
            }
            Err(e) => {
                tracing::warn!(
                    path = %note.path.display(),
                    error = %e,
                    "failed to process note with fabric"
                );
            }
        }
    }

    Ok(processed)
}

/// Generate a daily digest note.
///
/// Collects notes from the previous day (yesterday's ingestions) and summarizes them.
fn generate_daily_digest(vault_root: &Path, notes: &[Note], config: &IntelConfig, opts: &IntelOpts) -> Result<()> {
    let yesterday = Local::now().date_naive() - chrono::Duration::days(1);
    let yesterday_str = yesterday.format("%Y-%m-%d").to_string();
    let today = Local::now().format("%Y-%m-%d").to_string();
    tracing::info!(covering = %yesterday_str, "generating daily digest");

    // Find notes from yesterday (the day being digested)
    let recent_notes: Vec<&Note> = notes
        .iter()
        .filter(|n| {
            n.frontmatter
                .date
                .as_ref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                == Some(yesterday)
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

    digest.push_str("## Notes\n\n");
    if recent_notes.is_empty() {
        digest.push_str(&format!("No notes ingested on {yesterday_str}.\n\n"));
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
        "## Stats\n\n- Total vault notes: {}\n- Notes on {}: {}\n",
        notes.len(),
        yesterday_str,
        recent_notes.len()
    ));

    // Fabric enhancement: synthesize across all of yesterday's notes
    if let Some(ref pattern) = config.batch_daily
        && crate::fabric::is_available()
        && !recent_notes.is_empty()
    {
        let concatenated: String = recent_notes
            .iter()
            .map(|n| {
                let title = n.frontmatter.title.as_deref().unwrap_or("Untitled");
                format!("# {title}\n\n{}", n.body)
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        let input = crate::fabric::truncate_input(&concatenated, config.max_input_tokens);
        match crate::fabric::run_pattern(pattern, input, config.fabric_timeout_secs) {
            Ok(summary) => {
                digest.push_str("\n## AI Summary\n\n");
                digest.push_str(summary.trim());
                digest.push('\n');
            }
            Err(e) => {
                tracing::warn!(error = %e, "fabric daily summary failed, skipping");
            }
        }
    }

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

    // Fabric enhancement: synthesize across all of the week's notes
    if let Some(ref pattern) = config.batch_weekly
        && crate::fabric::is_available()
        && !week_notes.is_empty()
    {
        let concatenated: String = week_notes
            .iter()
            .map(|n| {
                let title = n.frontmatter.title.as_deref().unwrap_or("Untitled");
                format!("# {title}\n\n{}", n.body)
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");
        let input = crate::fabric::truncate_input(&concatenated, config.max_input_tokens);
        match crate::fabric::run_pattern(pattern, input, config.fabric_timeout_secs) {
            Ok(wisdom) => {
                review.push_str("## AI Insights\n\n");
                review.push_str(wisdom.trim());
                review.push('\n');
            }
            Err(e) => {
                tracing::warn!(error = %e, "fabric weekly insights failed, skipping");
            }
        }
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
    use crate::testutil::TestVault;

    #[test]
    fn test_daily_digest_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.intel;
        let opts = IntelOpts {
            daily: true,
            weekly: false,
            output: None,
        };

        run_intel(v.root(), &notes, &config, &opts).expect("run_intel");

        let today = Local::now().format("%Y-%m-%d").to_string();
        let digest_path = v.root().join("ai-output").join(format!("daily-{today}.md"));
        assert!(digest_path.exists());
        let content = std::fs::read_to_string(&digest_path).expect("read");
        assert!(content.contains("Daily Digest"));
        assert!(content.contains("Total vault notes:"));
    }

    #[test]
    fn test_weekly_review_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();
        let config = v.config().actions.intel;
        let opts = IntelOpts {
            daily: false,
            weekly: true,
            output: None,
        };

        run_intel(v.root(), &notes, &config, &opts).expect("run_intel");

        let output_dir = v.root().join("ai-output");
        assert!(output_dir.exists());
        let files: Vec<_> = std::fs::read_dir(&output_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("weekly-"))
            .collect();
        assert!(!files.is_empty());
    }

    #[test]
    fn test_resolve_output_path_explicit() {
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

    #[test]
    fn test_process_new_notes_skips_without_fabric() {
        let v = TestVault::new();
        v.add_note(
            "unread-note.md",
            "---\ntitle: Unread Note\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: assisted\nstatus: unread\ntags: []\n---\nSome content to process.\n",
        );
        let notes = v.scan();
        let config = IntelConfig {
            on_new_note: None, // Disabled
            ..Default::default()
        };

        let count = process_new_notes(v.root(), &notes, &config).expect("process");
        assert_eq!(count, 0, "should skip when on_new_note is None");
    }
}
