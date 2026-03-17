use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

/// Generate the after_help text with tool dependency checks and log path.
pub fn after_help_text() -> String {
    let fabric_status = check_tool("fabric", &["--version"]);
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("obsidian-cortex")
        .join("logs")
        .join("obsidian-cortex.log");

    format!(
        "REQUIRED TOOLS:\n  {fabric_status}\n\nLogs: {log_dir}",
        log_dir = log_dir.display()
    )
}

fn check_tool(name: &str, version_args: &[&str]) -> String {
    match ProcessCommand::new(name).args(version_args).output() {
        Ok(output) if output.status.success() => {
            let ver = String::from_utf8_lossy(&output.stdout)
                .trim()
                .lines()
                .next()
                .unwrap_or("unknown")
                .to_string();
            format!("  {name:<12} {ver}")
        }
        _ => format!("  {name:<12} NOT FOUND"),
    }
}

#[derive(Parser)]
#[command(
    name = "obsidian-cortex",
    about = "Vault governance and intelligence companion for Obsidian",
    version = env!("GIT_DESCRIBE"),
)]
pub struct Cli {
    /// Path to config file
    #[arg(short = 'c', long)]
    pub config: Option<PathBuf>,

    /// Vault root directory (default: CWD)
    #[arg(short = 'r', long = "vault")]
    pub vault: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long)]
    pub verbose: bool,

    /// Log level: trace, debug, info, warn, error
    /// Resolution: --log-level > OBSIDIAN_CORTEX_LOG env > config > info
    #[arg(short, long)]
    pub log_level: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Validate vault against rules
    Lint(LintOpts),
    /// Scan for and create wikilinks
    Link(LinkOpts),
    /// Generate intelligence (daily/weekly notes)
    Intel(IntelOpts),
    /// Vault state fingerprinting
    State(StateOpts),
    /// Watch mode - run actions on change
    Daemon(DaemonOpts),
    /// Schema evolution and vault structure migration
    Migrate(MigrateOpts),
}

#[derive(Parser)]
pub struct LintOpts {
    /// Report violations without fixing (default)
    #[arg(long, default_value_t = true)]
    pub dry_run: bool,

    /// Auto-fix what's fixable
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,

    /// Output format: human (default), json
    #[arg(long, default_value = "human")]
    pub format: String,

    /// Run only specific rule(s): naming, frontmatter, tags, scope, broken-links
    #[arg(long)]
    pub rule: Vec<String>,

    /// Lint only files matching glob pattern
    #[arg(long)]
    pub path: Option<String>,
}

#[derive(Parser)]
pub struct LinkOpts {
    /// Report suggested links without applying (default)
    #[arg(long, default_value_t = true)]
    pub dry_run: bool,

    /// Insert wikilinks into notes
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,

    /// What to scan for: people, projects, concepts, all (default)
    #[arg(long, default_value = "all")]
    pub scan: String,
}

#[derive(Parser)]
pub struct IntelOpts {
    /// Generate today's daily digest
    #[arg(long)]
    pub daily: bool,

    /// Generate weekly review
    #[arg(long)]
    pub weekly: bool,

    /// Write to specific path (default: vault daily note)
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Parser)]
pub struct StateOpts {
    /// Recompute and cache vault manifest
    #[arg(long)]
    pub refresh: bool,

    /// Show what changed since last cached manifest
    #[arg(long)]
    pub diff: bool,
}

#[derive(Parser)]
pub struct DaemonOpts {
    /// Install systemd user service
    #[arg(long)]
    pub install: bool,

    /// Remove systemd user service
    #[arg(long)]
    pub uninstall: bool,

    /// Start watching (used by systemd ExecStart)
    #[arg(long)]
    pub start: bool,

    /// Stop watching
    #[arg(long)]
    pub stop: bool,

    /// Show daemon status
    #[arg(long)]
    pub status: bool,
}

#[derive(Parser)]
pub struct MigrateOpts {
    /// Preview changes (default)
    #[arg(long, default_value_t = true)]
    pub dry_run: bool,

    /// Apply migrations
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,

    /// Path to migration plan YAML
    #[arg(long)]
    pub plan: Option<PathBuf>,
}
