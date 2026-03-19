#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]

use clap::{CommandFactory, FromArgMatches};
use eyre::{Context, Result};

use obsidian_cortex::cli::{self, Cli, Command};
use obsidian_cortex::config::Config;
use obsidian_cortex::logging;

#[tokio::main]
async fn main() -> Result<()> {
    // Augment clap with runtime after_help (tool checks + log path) before parsing
    let matches = Cli::command().after_help(cli::after_help_text()).get_matches();
    let cli = Cli::from_arg_matches(&matches).context("failed to parse arguments")?;

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
        Command::Link(opts) => {
            obsidian_cortex::run_link(&vault_root, &config, opts)?;
        }
        Command::Intel(opts) => {
            obsidian_cortex::run_intel(&vault_root, &config, opts)?;
        }
        Command::State(opts) => {
            obsidian_cortex::run_state(&vault_root, &config, opts)?;
        }
        Command::Daemon(opts) => {
            obsidian_cortex::daemon::run_daemon(&vault_root, &config, opts).await?;
        }
        Command::Migrate(opts) => {
            obsidian_cortex::run_migrate(&vault_root, &config, opts)?;
        }
    }

    Ok(())
}
