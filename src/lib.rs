pub mod cli;
pub mod config;
pub mod daemon;
pub mod duplicates;
pub mod fabric;
pub mod frontmatter;
pub mod intel;
pub mod linking;
pub mod links;
pub mod logging;
pub mod migrate;
pub mod naming;
pub mod report;
pub mod scope;
pub mod state;
pub mod tags;
pub mod testutil;
pub mod vault;

use colored::Colorize;
use eyre::Result;
use std::path::Path;
use tracing::instrument;

use cli::{IntelOpts, LinkOpts, LintOpts, MigrateOpts, StateOpts};
use config::Config;
use report::Report;
use state::VaultManifest;
use vault::scan_vault;

#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_lint(vault_root: &Path, config: &Config, opts: &LintOpts) -> Result<Report> {
    tracing::info!("starting lint run");
    let all_notes = scan_vault(vault_root, &config.vault)?;

    // Apply --path glob filter if provided
    let notes: Vec<_> = if let Some(ref pattern) = opts.path {
        let glob = glob::Pattern::new(pattern).map_err(|e| eyre::eyre!("invalid glob pattern '{}': {}", pattern, e))?;
        all_notes.into_iter().filter(|n| glob.matches_path(&n.path)).collect()
    } else {
        all_notes
    };

    tracing::info!(note_count = notes.len(), "vault scanned");

    let mut report = Report::default();

    let rules: Vec<&str> = if opts.rule.is_empty() {
        vec!["naming", "frontmatter", "tags", "scope", "broken-links"]
    } else {
        opts.rule.iter().map(|s| s.as_str()).collect()
    };

    tracing::info!(?rules, "running lint rules");

    if rules.contains(&"naming") {
        if opts.apply {
            naming::apply_naming(vault_root, &notes, &config.actions.naming)?;
        } else {
            report.merge(naming::lint_naming(&notes, &config.actions.naming));
        }
    }

    if rules.contains(&"frontmatter") {
        if opts.apply {
            frontmatter::apply_frontmatter(vault_root, &notes, &config.actions.frontmatter, &config.schema)?;
        } else {
            report.merge(frontmatter::lint_frontmatter(
                &notes,
                &config.actions.frontmatter,
                &config.schema,
            ));
        }
    }

    if rules.contains(&"tags") {
        if opts.apply {
            tags::apply_tags(vault_root, &notes, &config.actions.tags)?;
        } else {
            report.merge(tags::lint_tags(&notes, &config.actions.tags));
        }
    }

    if rules.contains(&"scope") {
        if opts.apply {
            scope::apply_scope(vault_root, &notes, &config.actions.scope)?;
        } else {
            report.merge(scope::lint_scope(&notes, &config.actions.scope));
        }
    }

    if rules.contains(&"broken-links") {
        report.merge(links::lint_broken_links(&notes, &config.actions.broken_links));
    }

    if rules.contains(&"duplicates") {
        report.merge(duplicates::lint_duplicates(&notes, &config.actions.duplicates));
    }

    if opts.format == "json" {
        report.print_json()?;
    } else {
        report.print_human();
    }

    Ok(report)
}

#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_state(vault_root: &Path, config: &Config, opts: &StateOpts) -> Result<()> {
    tracing::info!("starting state command");
    let cache_dir = &config.state.cache_dir;
    let manifest_path = VaultManifest::manifest_path(vault_root, cache_dir);

    if opts.refresh || opts.diff {
        let current = VaultManifest::scan(vault_root, &config.vault.ignore)?;

        if opts.diff {
            if manifest_path.exists() {
                let previous = VaultManifest::load(&manifest_path)?;
                let diff = previous.diff(&current);

                if diff.has_changes() {
                    if !diff.added.is_empty() {
                        println!("{}", "Added:".green().bold());
                        for p in &diff.added {
                            println!("  + {}", p.display());
                        }
                    }
                    if !diff.removed.is_empty() {
                        println!("{}", "Removed:".red().bold());
                        for p in &diff.removed {
                            println!("  - {}", p.display());
                        }
                    }
                    if !diff.modified.is_empty() {
                        println!("{}", "Modified:".yellow().bold());
                        for p in &diff.modified {
                            println!("  ~ {}", p.display());
                        }
                    }
                    println!(
                        "\n{}: {} added, {} removed, {} modified",
                        "Summary".bold(),
                        diff.added.len(),
                        diff.removed.len(),
                        diff.modified.len()
                    );
                } else {
                    println!("{}", "No changes since last scan.".green());
                }
            } else {
                println!("{}", "No previous manifest found. Run with --refresh first.".yellow());
            }
        }

        if opts.refresh {
            current.save(&manifest_path)?;
            println!(
                "{} manifest saved ({} files)",
                "Refreshed:".green().bold(),
                current.files.len()
            );
        }
    } else {
        // Default: show current manifest info
        if manifest_path.exists() {
            let manifest = VaultManifest::load(&manifest_path)?;
            println!(
                "Last scan: {} ({} files)",
                manifest.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                manifest.files.len()
            );
        } else {
            println!(
                "{}",
                "No manifest found. Run `cortex state --refresh` to create one.".yellow()
            );
        }
    }

    Ok(())
}

#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_migrate(vault_root: &Path, config: &Config, opts: &MigrateOpts) -> Result<Report> {
    tracing::info!("starting migrate command");
    let notes = scan_vault(vault_root, &config.vault)?;

    if opts.apply {
        let count = migrate::apply_migrate(vault_root, &notes, &config.migrations)?;
        println!("Migrated {count} file(s).");
        Ok(Report::default())
    } else {
        let report = migrate::lint_migrate(&notes, &config.migrations);
        report.print_human();
        Ok(report)
    }
}

#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_link(vault_root: &Path, config: &Config, opts: &LinkOpts) -> Result<Report> {
    tracing::info!("starting link command");
    let notes = scan_vault(vault_root, &config.vault)?;

    if opts.apply {
        let count = linking::apply_linking(vault_root, &notes, &config.actions.linking)?;
        println!("Inserted wikilinks in {count} file(s).");
        Ok(Report::default())
    } else {
        let report = linking::lint_linking(&notes, &config.actions.linking);
        report.print_human();
        Ok(report)
    }
}

#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_intel(vault_root: &Path, config: &Config, opts: &IntelOpts) -> Result<()> {
    tracing::info!("starting intel command");
    let notes = scan_vault(vault_root, &config.vault)?;
    intel::run_intel(vault_root, &notes, &config.actions.intel, opts)
}
