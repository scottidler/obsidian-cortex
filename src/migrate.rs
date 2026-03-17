use eyre::{Context, Result};
use glob::Pattern;
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

/// Run migration dry-run: report what would be moved.
#[instrument(skip(notes, migrations))]
pub fn lint_migrate(notes: &[Note], migrations: &[MigrationConfig]) -> Report {
    let mut report = Report::default();

    for migration in migrations {
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
    }

    tracing::info!(violation_count = report.violations.len(), "migrate lint complete");
    report
}

/// Apply migrations: move files and update frontmatter.
#[instrument(skip(notes, migrations))]
pub fn apply_migrate(vault_root: &Path, notes: &[Note], migrations: &[MigrationConfig]) -> Result<usize> {
    let mut move_count = 0;
    let mut all_moves: Vec<PlannedMove> = Vec::new();

    for migration in migrations {
        all_moves.extend(plan_migration(notes, migration));
    }

    if all_moves.is_empty() {
        return Ok(0);
    }

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

    Ok(move_count)
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

// Re-export insert_frontmatter_fields so it can be used by scope module
// (it's already in scope module, we just call it from there)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MigrationMove;
    use crate::vault::Frontmatter;
    use std::collections::HashMap;

    fn make_note(path: &str) -> Note {
        Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter::default(),
            body: String::new(),
            raw: String::new(),
        }
    }

    #[test]
    fn test_plan_migration_matches_glob() {
        let notes = vec![
            make_note("Tech/note-a.md"),
            make_note("Tech/note-b.md"),
            make_note("Work/note-c.md"),
        ];

        let migration = MigrationConfig {
            name: "flatten".to_string(),
            moves: vec![MigrationMove {
                from: "Tech/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
        };

        let moves = plan_migration(&notes, &migration);
        assert_eq!(moves.len(), 2);
        assert_eq!(moves[0].to, PathBuf::from("Notes/note-a.md"));
        assert_eq!(moves[1].to, PathBuf::from("Notes/note-b.md"));
    }

    #[test]
    fn test_plan_migration_no_match() {
        let notes = vec![make_note("Other/note.md")];

        let migration = MigrationConfig {
            name: "noop".to_string(),
            moves: vec![MigrationMove {
                from: "Tech/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
        };

        let moves = plan_migration(&notes, &migration);
        assert!(moves.is_empty());
    }

    #[test]
    fn test_plan_migration_with_frontmatter() {
        let notes = vec![make_note("Work/meeting.md")];

        let mut fm_set = HashMap::new();
        fm_set.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));

        let migration = MigrationConfig {
            name: "scope-work".to_string(),
            moves: vec![MigrationMove {
                from: "Work/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: Some(fm_set),
            }],
        };

        let moves = plan_migration(&notes, &migration);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].set_frontmatter.len(), 1);
    }

    #[test]
    fn test_lint_migrate_reports_moves() {
        let notes = vec![make_note("Tech/note.md")];

        let migrations = vec![MigrationConfig {
            name: "test".to_string(),
            moves: vec![MigrationMove {
                from: "Tech/**".to_string(),
                to: "Notes/".to_string(),
                set_frontmatter: None,
            }],
        }];

        let report = lint_migrate(&notes, &migrations);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].rule, "migrate.test");
    }
}
