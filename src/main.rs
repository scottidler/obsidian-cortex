#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::Parser;
use eyre::{Context, Result};

use obsidian_cortex::cli::{Cli, Command};
use obsidian_cortex::config::Config;
use obsidian_cortex::logging;

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load config first (needed for log level resolution)
    let config = Config::load(cli.config.as_ref()).context("failed to load configuration")?;

    // Resolve and setup logging
    let level = logging::resolve_log_level(cli.log_level.as_deref(), &config.log_level);
    logging::setup_tracing(&level)?;

    tracing::info!(version = env!("GIT_DESCRIBE"), "obsidian-cortex starting");

    let vault_root = config.vault_root(cli.vault.as_ref());
    tracing::info!(vault_root = %vault_root.display(), "resolved vault root");

    match &cli.command {
        Command::Lint(opts) => {
            obsidian_cortex::run_lint(&vault_root, &config, opts)?;
        }
        Command::Link(..) => {
            println!("Link command not yet implemented (Phase 2)");
        }
        Command::Intel(..) => {
            println!("Intel command not yet implemented (Phase 2)");
        }
        Command::State(opts) => {
            obsidian_cortex::run_state(&vault_root, &config, opts)?;
        }
        Command::Daemon(..) => {
            println!("Daemon command not yet implemented (Phase 2)");
        }
        Command::Migrate(opts) => {
            obsidian_cortex::run_migrate(&vault_root, &config, opts)?;
        }
    }

    Ok(())
}
