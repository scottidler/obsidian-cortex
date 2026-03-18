use eyre::{Context, Result};
use glob::Pattern;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::instrument;

use crate::config::MigrationConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Planned file move with optional frontmatter updates.
#[derive(Debug)]
struct PlannedMove {
    from: PathBuf,
    to: PathBuf,
    set_frontmatter: Vec<(String, serde_yaml::Value)>,
}

/// Run migration dry-run: report what would be moved and what fields would change.
#[instrument(skip(notes, migrations))]
pub fn lint_migrate(notes: &[Note], migrations: &[MigrationConfig]) -> Report {
    let mut report = Report::default();

    for migration in migrations {
        // Report file moves
        let moves = plan_migration(notes, migration);
        for planned in &moves {
            report.add(Violation {
                path: planned.from.clone(),
                rule: format!("migrate.{}", migration.name),
                severity: Severity::Info,
                message: format!("would move to {}", planned.to.display()),
                fix: Some(Fix::MoveFile {
                    from: planned.from.clone(),
                    to: planned.to.clone(),
                }),
            });
        }

        // Report field transforms
        lint_field_transforms(notes, migration, &mut report);
    }

    tracing::info!(violation_count = report.violations.len(), "migrate lint complete");
    report
}

/// Apply migrations: field transforms first, then file moves.
#[instrument(skip(notes, migrations))]
pub fn apply_migrate(vault_root: &Path, notes: &[Note], migrations: &[MigrationConfig]) -> Result<usize> {
    let mut total_count = 0;

    // Phase 1: Apply field transforms (renames and drops)
    for migration in migrations {
        if !migration.field_renames.is_empty() || !migration.field_drops.is_empty() {
            let count = apply_field_transforms(vault_root, notes, migration)?;
            total_count += count;
        }
    }

    // Phase 2: Apply file moves
    let mut all_moves: Vec<PlannedMove> = Vec::new();
    for migration in migrations {
        all_moves.extend(plan_migration(notes, migration));
    }

    if all_moves.is_empty() {
        return Ok(total_count);
    }

    let mut move_count = 0;

    // Execute moves
    for planned in &all_moves {
        let abs_from = vault_root.join(&planned.from);
        let abs_to = vault_root.join(&planned.to);

        if let Some(parent) = abs_to.parent() {
            std::fs::create_dir_all(parent).context(format!("failed to create directory {}", parent.display()))?;
        }

        std::fs::rename(&abs_from, &abs_to).context(format!(
            "failed to move {} to {}",
            abs_from.display(),
            abs_to.display()
        ))?;

        // Apply frontmatter updates if any
        if !planned.set_frontmatter.is_empty() {
            let content = std::fs::read_to_string(&abs_to)?;
            if let Some(new_content) = crate::scope::insert_frontmatter_fields(&content, &planned.set_frontmatter) {
                std::fs::write(&abs_to, new_content)?;
            }
        }

        tracing::info!(
            from = %planned.from.display(),
            to = %planned.to.display(),
            "migrated file"
        );
        move_count += 1;
    }

    // Batch update wikilinks for all moves
    let renames: Vec<(PathBuf, PathBuf)> = all_moves.iter().map(|m| (m.from.clone(), m.to.clone())).collect();
    update_wikilinks_for_moves(vault_root, notes, &renames)?;

    Ok(total_count + move_count)
}

/// Plan all moves for a single migration config.
fn plan_migration(notes: &[Note], migration: &MigrationConfig) -> Vec<PlannedMove> {
    let mut moves = Vec::new();

    for move_rule in &migration.moves {
        let pattern = match Pattern::new(&move_rule.from) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(pattern = %move_rule.from, error = %e, "invalid glob pattern");
                continue;
            }
        };

        for note in notes {
            let path_str = note.path.to_string_lossy();
            if pattern.matches(&path_str) {
                let filename = match note.path.file_name() {
                    Some(f) => f,
                    None => continue,
                };

                let to = PathBuf::from(&move_rule.to).join(filename);

                // Don't plan a move if source == destination
                if note.path == to {
                    continue;
                }

                let set_frontmatter = move_rule
                    .set_frontmatter
                    .as_ref()
                    .map(|fm| fm.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();

                moves.push(PlannedMove {
                    from: note.path.clone(),
                    to,
                    set_frontmatter,
                });
            }
        }
    }

    moves
}

/// Update wikilinks across the vault after file moves.
fn update_wikilinks_for_moves(vault_root: &Path, notes: &[Note], renames: &[(PathBuf, PathBuf)]) -> Result<()> {
    if renames.is_empty() {
        return Ok(());
    }

    let rename_map: Vec<(String, String)> = renames
        .iter()
        .filter_map(|(from, to)| {
            let old_stem = from.file_stem()?.to_str()?.to_string();
            let new_stem = to.file_stem()?.to_str()?.to_string();
            // Only update if the stem actually changed
            if old_stem != new_stem { Some((old_stem, new_stem)) } else { None }
        })
        .collect();

    if rename_map.is_empty() {
        return Ok(());
    }

    // Use the same batch approach as naming
    let moved_paths: std::collections::HashSet<&PathBuf> = renames.iter().map(|(from, _)| from).collect();

    for note in notes {
        if moved_paths.contains(&note.path) {
            continue;
        }

        let abs_path = vault_root.join(&note.path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut new_content = content.clone();

        for (old_stem, new_stem) in &rename_map {
            let pattern = format!(r"\[\[(?i){}\]\]", regex::escape(old_stem));
            if let Ok(re) = regex::Regex::new(&pattern) {
                new_content = re.replace_all(&new_content, format!("[[{new_stem}]]")).to_string();
            }
        }

        if new_content != content {
            std::fs::write(&abs_path, &new_content)?;
            tracing::info!(path = %note.path.display(), "updated wikilinks after migration");
        }
    }

    Ok(())
}

/// Report what field transforms would be applied (dry-run).
fn lint_field_transforms(notes: &[Note], migration: &MigrationConfig, report: &mut Report) {
    if migration.field_renames.is_empty() && migration.field_drops.is_empty() {
        return;
    }

    for note in notes {
        for (old_key, new_key) in &migration.field_renames {
            if note.frontmatter.extra.contains_key(old_key) {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: format!("migrate.{}.rename", migration.name),
                    severity: Severity::Info,
                    message: format!("would rename field '{old_key}' to '{new_key}'"),
                    fix: None,
                });
            }
        }
        for drop_key in &migration.field_drops {
            if note.frontmatter.extra.contains_key(drop_key) {
                report.add(Violation {
                    path: note.path.clone(),
                    rule: format!("migrate.{}.drop", migration.name),
                    severity: Severity::Info,
                    message: format!("would drop field '{drop_key}'"),
                    fix: None,
                });
            }
        }
    }
}

/// Apply field renames and drops within frontmatter blocks.
/// Operates on the raw text between `---` delimiters to preserve formatting.
fn apply_field_transforms(vault_root: &Path, notes: &[Note], migration: &MigrationConfig) -> Result<usize> {
    let mut count = 0;

    for note in notes {
        // Quick check: does this note have any fields to transform?
        let has_rename_target = migration
            .field_renames
            .keys()
            .any(|k| note.frontmatter.extra.contains_key(k));
        let has_drop_target = migration
            .field_drops
            .iter()
            .any(|k| note.frontmatter.extra.contains_key(k));

        if !has_rename_target && !has_drop_target {
            continue;
        }

        let abs_path = vault_root.join(&note.path);
        let content = std::fs::read_to_string(&abs_path).context(format!("failed to read {}", abs_path.display()))?;

        let Some((fm_block, before, after)) = extract_frontmatter_block(&content) else {
            continue;
        };

        let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
        let mut changed = false;

        // Build set of existing keys for conflict detection
        let existing_keys: HashSet<String> = lines
            .iter()
            .filter_map(|l| l.split(':').next().map(|k| k.trim().to_string()))
            .collect();

        // Apply renames
        for (old_key, new_key) in &migration.field_renames {
            for line in &mut lines {
                if line.starts_with(&format!("{old_key}:")) {
                    if existing_keys.contains(new_key) {
                        tracing::warn!(
                            path = %note.path.display(),
                            old_key,
                            new_key,
                            "skipping rename: target field already exists"
                        );
                    } else {
                        *line = line.replacen(old_key, new_key, 1);
                        changed = true;
                    }
                }
            }
        }

        // Apply drops
        let original_len = lines.len();
        lines.retain(|line| {
            !migration
                .field_drops
                .iter()
                .any(|dk| line.starts_with(&format!("{dk}:")))
        });
        if lines.len() != original_len {
            changed = true;
        }

        if changed {
            let new_content = format!("{before}---\n{}\n---{after}", lines.join("\n"));
            std::fs::write(&abs_path, new_content)?;
            tracing::info!(path = %note.path.display(), "applied field transforms");
            count += 1;
        }
    }

    Ok(count)
}

/// Extract the frontmatter block from file content.
/// Returns (frontmatter_text, content_before_opening_delim, content_after_closing_delim).
fn extract_frontmatter_block(content: &str) -> Option<(&str, &str, &str)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let before_offset = content.len() - trimmed.len();
    let before = &content[..before_offset];

    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);

    let end_pos = after_opening.find("\n---")?;
    let fm_block = &after_opening[..end_pos];
    let after = &after_opening[end_pos + 4..]; // skip \n---

    Some((fm_block, before, after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MigrationMove;
    use crate::testutil::TestVault;
    use std::collections::HashMap;

    #[test]
    fn test_plan_migration_on_vault() {
        let v = TestVault::new();
        let notes = v.scan();

        let migration = MigrationConfig {
            name: "flatten-projects".to_string(),
            moves: vec![MigrationMove {
                from: "projects/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
            ..Default::default()
        };

        let moves = plan_migration(&notes, &migration);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].to, PathBuf::from("Notes/obsidian-cortex.md"));
    }

    #[test]
    fn test_plan_migration_no_match() {
        let v = TestVault::new();
        let notes = v.scan();

        let migration = MigrationConfig {
            name: "noop".to_string(),
            moves: vec![MigrationMove {
                from: "nonexistent/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
            ..Default::default()
        };

        let moves = plan_migration(&notes, &migration);
        assert!(moves.is_empty());
    }

    #[test]
    fn test_plan_migration_with_frontmatter_set() {
        let v = TestVault::new();
        let notes = v.scan();

        let mut fm_set = HashMap::new();
        fm_set.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));

        let migration = MigrationConfig {
            name: "scope-projects".to_string(),
            moves: vec![MigrationMove {
                from: "projects/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: Some(fm_set),
            }],
            ..Default::default()
        };

        let moves = plan_migration(&notes, &migration);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].set_frontmatter.len(), 1);
    }

    #[test]
    fn test_apply_migrate_moves_files() {
        let v = TestVault::new();
        let notes = v.scan();

        let migrations = vec![MigrationConfig {
            name: "flatten".to_string(),
            moves: vec![MigrationMove {
                from: "projects/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
            ..Default::default()
        }];

        let count = apply_migrate(v.root(), &notes, &migrations).expect("apply");
        assert_eq!(count, 1);
        assert!(v.exists("Notes/obsidian-cortex.md"));
        assert!(!v.exists("projects/obsidian-cortex.md"));
    }

    #[test]
    fn test_lint_migrate_reports_moves() {
        let v = TestVault::new();
        let notes = v.scan();

        let migrations = vec![MigrationConfig {
            name: "test".to_string(),
            moves: vec![MigrationMove {
                from: "projects/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
            ..Default::default()
        }];

        let report = lint_migrate(&notes, &migrations);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule, "migrate.test");
    }

    #[test]
    fn test_field_rename_applies() {
        let v = TestVault::new();
        let notes = v.scan();

        let mut renames = HashMap::new();
        renames.insert("url".to_string(), "source".to_string());
        renames.insert("author".to_string(), "creator".to_string());

        let migration = MigrationConfig {
            name: "v2-renames".to_string(),
            field_renames: renames,
            ..Default::default()
        };

        let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
        assert!(count > 0, "expected at least one file transformed");

        // legacy-note.md had url and author, should now have source and creator
        let content = v.read("legacy-note.md");
        assert!(content.contains("source:"), "expected 'source:' after rename");
        assert!(content.contains("creator:"), "expected 'creator:' after rename");
        assert!(!content.contains("\nurl:"), "expected 'url:' to be renamed");
        assert!(!content.contains("\nauthor:"), "expected 'author:' to be renamed");
    }

    #[test]
    fn test_field_drop_applies() {
        let v = TestVault::new();
        // Add a note with droppable fields
        v.add_note(
            "drop-test.md",
            "---\ntitle: Drop Test\ndate: 2026-01-01\ntype: note\nday: monday\ntime: 10:00\ntags: []\n---\nBody.\n",
        );
        let notes = v.scan();

        let migration = MigrationConfig {
            name: "v2-drops".to_string(),
            field_drops: vec!["day".to_string(), "time".to_string()],
            ..Default::default()
        };

        let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
        assert!(count > 0);

        let content = v.read("drop-test.md");
        assert!(!content.contains("day:"));
        assert!(!content.contains("time:"));
        assert!(content.contains("title: Drop Test"));
    }

    #[test]
    fn test_field_rename_skips_conflict() {
        let v = TestVault::new();
        // Note already has both 'author' and 'creator'
        v.add_note(
            "conflict-note.md",
            "---\ntitle: Conflict\ndate: 2026-01-01\ntype: note\nauthor: Old Author\ncreator: Existing Creator\ntags: []\n---\nBody.\n",
        );
        let notes = v.scan();

        let mut renames = HashMap::new();
        renames.insert("author".to_string(), "creator".to_string());

        let migration = MigrationConfig {
            name: "v2-renames".to_string(),
            field_renames: renames,
            ..Default::default()
        };

        let count = apply_field_transforms(v.root(), &notes, &migration).expect("apply");
        // Should skip due to conflict - creator already exists
        let content = v.read("conflict-note.md");
        assert!(
            content.contains("author: Old Author"),
            "author should be preserved due to conflict"
        );
        assert!(content.contains("creator: Existing Creator"));
        // The conflict note should not count as transformed since it was skipped
        // (legacy-note.md may also get transformed, so count could be > 0)
        let _ = count;
    }

    #[test]
    fn test_lint_field_transforms_reports() {
        let v = TestVault::new();
        let notes = v.scan();

        let mut renames = HashMap::new();
        renames.insert("url".to_string(), "source".to_string());

        let migrations = vec![MigrationConfig {
            name: "v2".to_string(),
            field_renames: renames,
            field_drops: vec!["folder".to_string()],
            ..Default::default()
        }];

        let report = lint_migrate(&notes, &migrations);
        // legacy-note.md has both url and folder
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule.contains("rename") && v.path.to_string_lossy() == "legacy-note.md")
        );
        assert!(
            report
                .violations
                .iter()
                .any(|v| v.rule.contains("drop") && v.path.to_string_lossy() == "legacy-note.md")
        );
    }

    #[test]
    fn test_extract_frontmatter_block() {
        let content = "---\ntitle: Test\ndate: 2026-01-01\n---\nBody here.\n";
        let (fm, before, after) = extract_frontmatter_block(content).expect("extract");
        assert!(fm.contains("title: Test"));
        assert!(fm.contains("date: 2026-01-01"));
        assert_eq!(before, "");
        assert!(after.contains("Body here."));
    }
}
