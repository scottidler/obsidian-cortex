use eyre::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tracing::instrument;

use crate::cli::DaemonOpts;
use crate::config::{Config, DaemonConfig};

/// Run the daemon based on subcommand options.
#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub fn run_daemon(vault_root: &Path, config: &Config, opts: &DaemonOpts) -> Result<()> {
    if opts.install {
        install_systemd_service(vault_root, config)?;
    } else if opts.uninstall {
        uninstall_systemd_service()?;
    } else if opts.status {
        show_status()?;
    } else if opts.stop {
        println!("Send SIGTERM to the running daemon process to stop it.");
    } else {
        // Default: start watching (--start or no flags)
        start_watching(vault_root, config)?;
    }
    Ok(())
}

/// Start filesystem watcher and run actions on changes.
fn start_watching(vault_root: &Path, config: &Config) -> Result<()> {
    let daemon_config = &config.daemon;
    let debounce = Duration::from_secs(daemon_config.debounce_secs);

    let action_names: Vec<&str> = daemon_config.enabled_actions();
    let any_auto_apply = daemon_config.actions.values().any(|a| a.apply);

    println!("Starting daemon, watching: {}", vault_root.display());
    println!(
        "Debounce: {}s, actions: {}{}",
        daemon_config.debounce_secs,
        action_names.join(", "),
        if any_auto_apply { " (auto-apply enabled)" } else { "" },
    );

    // Flag to suppress events during auto-apply to prevent feedback loops.
    let applying = Arc::new(AtomicBool::new(false));
    let applying_clone = Arc::clone(&applying);

    let (tx, rx) = mpsc::channel();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if applying_clone.load(Ordering::Relaxed) {
                return; // Discard events during auto-apply
            }
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        })
        .context("failed to create filesystem watcher")?;

    watcher
        .watch(vault_root.as_ref(), RecursiveMode::Recursive)
        .context("failed to watch vault root")?;

    tracing::info!(vault_root = %vault_root.display(), "daemon started");

    let mut last_run = Instant::now() - debounce; // Allow immediate first run
    let mut pending_changes: Vec<PathBuf> = Vec::new();

    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                if should_process_event(&event, &config.vault.ignore) {
                    for path in event.paths {
                        if path.extension().and_then(|e| e.to_str()) == Some("md") {
                            let relative = path.strip_prefix(vault_root).unwrap_or(&path).to_path_buf();
                            if !pending_changes.contains(&relative) {
                                pending_changes.push(relative);
                            }
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Check if we should flush pending changes
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("watcher channel disconnected");
                break;
            }
        }

        // Debounce: run actions if enough time has passed since last run
        if !pending_changes.is_empty() && last_run.elapsed() >= debounce {
            tracing::info!(changed_files = pending_changes.len(), "processing changes");

            for path in &pending_changes {
                println!("  changed: {}", path.display());
            }

            applying.store(true, Ordering::Relaxed);
            run_configured_actions(vault_root, config, daemon_config, &pending_changes);
            applying.store(false, Ordering::Relaxed);
            pending_changes.clear();
            last_run = Instant::now();
        }
    }

    Ok(())
}

/// Check if a filesystem event should be processed.
fn should_process_event(event: &notify::Event, ignore_dirs: &[String]) -> bool {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return false,
    }

    // Check if any path is in an ignored directory
    for path in &event.paths {
        for component in path.components() {
            let name = component.as_os_str().to_string_lossy();
            if ignore_dirs.iter().any(|ig| name == *ig) {
                return false;
            }
        }
    }

    true
}

/// Run the configured on-change actions.
fn run_configured_actions(vault_root: &Path, config: &Config, daemon_config: &DaemonConfig, changed_files: &[PathBuf]) {
    let action_names: Vec<&str> = daemon_config.enabled_actions();
    tracing::info!(actions = ?action_names, "running configured actions");

    for action in &action_names {
        match *action {
            "lint" => {
                let auto = daemon_config.should_apply("lint");
                let opts = crate::cli::LintOpts {
                    apply: auto,
                    format: "human".to_string(),
                    rule: Vec::new(),
                    path: None,
                };
                match crate::run_lint(vault_root, config, &opts) {
                    Ok(report) => {
                        let count = report.violations.len();
                        if auto && count > 0 {
                            tracing::info!(fixes = count, "auto-applied lint");
                            println!("[daemon] auto-applied lint: {count} fix(es)");
                        } else if !report.is_empty() {
                            println!("[daemon] lint: {count} violation(s)");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "lint action failed"),
                }
            }
            "broken-links" => {
                let notes = match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(n) => n,
                    Err(e) => {
                        tracing::error!(error = %e, "failed to scan vault for broken links");
                        continue;
                    }
                };
                let report = crate::links::lint_broken_links(&notes, &notes, &config.actions.broken_links);
                if !report.is_empty() {
                    println!("[daemon] broken-links: {} violation(s)", report.violations.len());
                }
            }
            "link" => {
                let auto = daemon_config.should_apply("link");
                let opts = crate::cli::LinkOpts {
                    apply: auto,
                    scan: "all".to_string(),
                };
                match crate::run_link(vault_root, config, &opts) {
                    Ok(report) => {
                        let count = report.violations.len();
                        if auto && count > 0 {
                            tracing::info!(fixes = count, "auto-applied link");
                            println!("[daemon] auto-applied link: {count} fix(es)");
                        } else if !report.is_empty() {
                            println!("[daemon] link: {count} suggestion(s)");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "link action failed"),
                }
            }
            "intel" => {
                let opts = crate::cli::IntelOpts {
                    daily: true,
                    weekly: false,
                    output: None,
                };
                if let Err(e) = crate::run_intel(vault_root, config, &opts) {
                    tracing::error!(error = %e, "intel action failed");
                }
            }
            "state" => {
                let opts = crate::cli::StateOpts {
                    refresh: true,
                    diff: false,
                };
                if let Err(e) = crate::run_state(vault_root, config, &opts) {
                    tracing::error!(error = %e, "state action failed");
                }
            }
            other => {
                tracing::warn!(action = %other, "unknown daemon action");
            }
        }
    }

    tracing::info!(changed_count = changed_files.len(), "daemon action cycle complete");
}

/// Install a systemd user service for the daemon.
fn install_systemd_service(vault_root: &Path, config: &Config) -> Result<()> {
    let service_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user");

    std::fs::create_dir_all(&service_dir).context("failed to create systemd user dir")?;

    let binary = std::env::current_exe().context("failed to get current executable path")?;
    let vault = vault_root.display();

    let mut config_flag = String::new();
    if let Some(config_dir) = dirs::config_dir() {
        let config_path = config_dir.join("obsidian-cortex").join("obsidian-cortex.yml");
        if config_path.exists() {
            config_flag = format!(" --config {}", config_path.display());
        }
    }

    let log_level = &config.log_level;

    let service = format!(
        "[Unit]\n\
         Description=Obsidian Cortex Vault Daemon\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={binary}{config_flag} --vault {vault} --log-level {log_level} daemon --start\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        binary = binary.display(),
    );

    let service_path = service_dir.join("obsidian-cortex.service");
    std::fs::write(&service_path, service)?;

    println!("Installed: {}", service_path.display());
    println!("Run: systemctl --user daemon-reload && systemctl --user enable --now obsidian-cortex");

    Ok(())
}

/// Uninstall the systemd user service.
fn uninstall_systemd_service() -> Result<()> {
    let service_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user")
        .join("obsidian-cortex.service");

    if service_path.exists() {
        std::fs::remove_file(&service_path)?;
        println!("Removed: {}", service_path.display());
        println!("Run: systemctl --user daemon-reload");
    } else {
        println!("No service file found at {}", service_path.display());
    }

    Ok(())
}

/// Show daemon status.
fn show_status() -> Result<()> {
    let service_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user")
        .join("obsidian-cortex.service");

    if service_path.exists() {
        println!("Service file: {}", service_path.display());
        println!("Check status: systemctl --user status obsidian-cortex");
    } else {
        println!("Daemon not installed. Run: cortex daemon --install");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;

    #[test]
    fn test_should_apply_default_is_false() {
        let config = DaemonConfig::default();
        assert!(!config.should_apply("lint"));
        assert!(!config.should_apply("link"));
        assert!(!config.should_apply("nonexistent"));
    }

    #[test]
    fn test_should_apply_explicit_true() {
        let mut config = DaemonConfig::default();
        config
            .actions
            .insert("lint".to_string(), crate::config::DaemonAction { apply: true });
        assert!(config.should_apply("lint"));
        assert!(!config.should_apply("link"));
    }

    #[test]
    fn test_should_apply_explicit_false() {
        let config = DaemonConfig::default();
        // lint is in default actions but apply defaults to false
        assert!(!config.should_apply("lint"));
    }

    #[test]
    fn test_enabled_actions() {
        let config = DaemonConfig::default();
        let actions = config.enabled_actions();
        assert!(actions.contains(&"lint"));
        assert!(actions.contains(&"broken-links"));
    }

    #[test]
    fn test_daemon_config_deserialize_actions() {
        let yaml =
            "actions:\n  lint:\n    apply: true\n  broken-links: {}\n  link:\n    apply: false\ndebounce-secs: 10\n";
        let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.debounce_secs, 10);
        assert!(config.should_apply("lint"));
        assert!(!config.should_apply("broken-links"));
        assert!(!config.should_apply("link"));
        assert!(!config.should_apply("nonexistent"));
        assert_eq!(config.actions.len(), 3);
    }

    #[test]
    fn test_should_process_event_create() {
        let event = notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![PathBuf::from("/vault/note.md")],
            attrs: Default::default(),
        };
        assert!(should_process_event(&event, &[]));
    }

    #[test]
    fn test_should_process_event_ignores_git() {
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(notify::event::DataChange::Content)),
            paths: vec![PathBuf::from("/vault/.git/objects/abc")],
            attrs: Default::default(),
        };
        assert!(!should_process_event(&event, &[".git".to_string()]));
    }

    #[test]
    fn test_should_process_event_ignores_access() {
        let event = notify::Event {
            kind: EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![PathBuf::from("/vault/note.md")],
            attrs: Default::default(),
        };
        assert!(!should_process_event(&event, &[]));
    }
}
