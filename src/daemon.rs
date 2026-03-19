use chrono::Datelike;
use eyre::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;
use tracing::instrument;

use crate::cli::DaemonOpts;
use crate::config::{Config, DaemonConfig};

/// Fingerprint of a single sweep's apply results.
/// Used to detect oscillation between consecutive sweeps.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SweepFingerprint {
    /// Sorted list of (action, sorted file paths) for actions that applied changes.
    results: Vec<(String, Vec<String>)>,
}

impl SweepFingerprint {
    fn is_empty(&self) -> bool {
        self.results.is_empty() || self.results.iter().all(|(_, files)| files.is_empty())
    }

    fn add(&mut self, action: &str, mut files: Vec<String>) {
        files.sort();
        files.dedup();
        if !files.is_empty() {
            self.results.push((action.to_string(), files));
        }
    }
}

/// Run the daemon based on subcommand options.
#[instrument(skip(config, opts), fields(vault_root = %vault_root.display()))]
pub async fn run_daemon(vault_root: &Path, config: &Config, opts: &DaemonOpts) -> Result<()> {
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
        start_watching(vault_root, config).await?;
    }
    Ok(())
}

/// Start filesystem watcher and run actions on changes using async tokio::select! loop.
async fn start_watching(vault_root: &Path, config: &Config) -> Result<()> {
    let daemon_config = &config.daemon;
    let debounce_duration = Duration::from_secs(daemon_config.debounce_secs);
    let poll_interval = Duration::from_secs(daemon_config.poll_interval);

    let action_names: Vec<&str> = daemon_config.enabled_actions();
    let any_enabled = daemon_config.actions.values().any(|a| a.enable);

    println!("Starting daemon, watching: {}", vault_root.display());
    println!(
        "Debounce: {}s, actions: {}{}",
        daemon_config.debounce_secs,
        action_names.join(", "),
        if any_enabled { " (auto-apply enabled)" } else { "" },
    );

    // Channel for file watcher events -> async event loop
    let (watch_tx, mut watch_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let _ = watch_tx.send(event);
            }
        })
        .context("failed to create filesystem watcher")?;

    watcher
        .watch(vault_root.as_ref(), RecursiveMode::Recursive)
        .context("failed to watch vault root")?;

    tracing::info!(vault_root = %vault_root.display(), "daemon started");

    // Timers
    let mut sweep_interval = tokio::time::interval(poll_interval);
    sweep_interval.tick().await; // consume the immediate first tick

    // Debounce: starts inert (far future), reset when events arrive
    let debounce = tokio::time::sleep(Duration::MAX);
    tokio::pin!(debounce);

    // Scheduled intel timers
    let intel_enabled = daemon_config.is_enabled("intel");
    let daily_dur = match (&daemon_config.daily_at, intel_enabled) {
        (Some(time_str), true) => {
            let dur = duration_until_daily(time_str);
            println!(
                "Daily intel scheduled at {time_str} (in {:.0}m)",
                dur.as_secs_f64() / 60.0
            );
            dur
        }
        _ => Duration::MAX, // inert
    };
    let daily = tokio::time::sleep(daily_dur);
    tokio::pin!(daily);

    let weekly_dur = match (&daemon_config.weekly_on, intel_enabled) {
        (Some(schedule_str), true) => {
            let dur = duration_until_weekly(schedule_str);
            println!(
                "Weekly intel scheduled for {schedule_str} (in {:.1}h)",
                dur.as_secs_f64() / 3600.0
            );
            dur
        }
        _ => Duration::MAX, // inert
    };
    let weekly = tokio::time::sleep(weekly_dur);
    tokio::pin!(weekly);

    let mut pending: Vec<PathBuf> = Vec::new();

    // Run a full sweep on startup
    tracing::info!("running initial full sweep");
    let mut last_fingerprint = run_configured_actions(vault_root, config, daemon_config, &[]);

    loop {
        tokio::select! {
            Some(event) = watch_rx.recv() => {
                if should_process_event(&event, &config.vault.ignore) {
                    // Real user edit - reset cycle detection
                    last_fingerprint = SweepFingerprint::default();
                    for path in event.paths {
                        if path.extension().and_then(|e| e.to_str()) == Some("md") {
                            let relative = path.strip_prefix(vault_root).unwrap_or(&path).to_path_buf();
                            if !pending.contains(&relative) {
                                pending.push(relative);
                            }
                        }
                    }
                    // Reset debounce timer
                    debounce.as_mut().reset(Instant::now() + debounce_duration);
                }
            }
            () = &mut debounce, if !pending.is_empty() => {
                // Debounce fired - process pending changes
                tracing::info!(changed_files = pending.len(), "processing changes");
                for path in &pending {
                    println!("  changed: {}", path.display());
                }
                let fingerprint = run_configured_actions(vault_root, config, daemon_config, &pending);
                last_fingerprint = fingerprint;
                pending.clear();
                // Make debounce inert again
                debounce.as_mut().reset(Instant::now() + Duration::MAX);
                // Reset sweep interval after processing changes
                sweep_interval.reset();
            }
            _ = sweep_interval.tick() => {
                // Periodic full sweep with cycle detection
                tracing::info!("running periodic sweep");
                let fingerprint = run_configured_actions(vault_root, config, daemon_config, &[]);

                if !fingerprint.is_empty() && fingerprint == last_fingerprint {
                    tracing::warn!(
                        actions = ?fingerprint.results.iter().map(|(a, f)| format!("{a}: {} files", f.len())).collect::<Vec<_>>(),
                        "cycle detected: sweep produced same results as previous, backing off"
                    );
                }
                last_fingerprint = fingerprint;
            }
            () = &mut daily => {
                // Scheduled daily intel
                tracing::info!("running scheduled daily intel");
                println!("[daemon] running scheduled daily intel");
                let opts = crate::cli::IntelOpts {
                    daily: true,
                    weekly: false,
                    output: None,
                };
                if let Err(e) = crate::run_intel(vault_root, config, &opts) {
                    tracing::error!(error = %e, "scheduled daily intel failed");
                }
                // Reschedule for next day
                if let Some(time_str) = &daemon_config.daily_at {
                    let next = duration_until_daily(time_str);
                    tracing::info!(next_in_secs = next.as_secs(), "daily intel rescheduled");
                    daily.as_mut().reset(Instant::now() + next);
                }
            }
            () = &mut weekly => {
                // Scheduled weekly intel
                tracing::info!("running scheduled weekly intel");
                println!("[daemon] running scheduled weekly intel");
                let opts = crate::cli::IntelOpts {
                    daily: false,
                    weekly: true,
                    output: None,
                };
                if let Err(e) = crate::run_intel(vault_root, config, &opts) {
                    tracing::error!(error = %e, "scheduled weekly intel failed");
                }
                // Reschedule for next week
                if let Some(schedule_str) = &daemon_config.weekly_on {
                    let next = duration_until_weekly(schedule_str);
                    tracing::info!(next_in_secs = next.as_secs(), "weekly intel rescheduled");
                    weekly.as_mut().reset(Instant::now() + next);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received shutdown signal");
                println!("\nShutting down daemon...");
                break;
            }
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

/// Run the configured on-change actions, returning a fingerprint of what was applied.
fn run_configured_actions(
    vault_root: &Path,
    config: &Config,
    daemon_config: &DaemonConfig,
    changed_files: &[PathBuf],
) -> SweepFingerprint {
    let action_names: Vec<&str> = daemon_config.enabled_actions();
    tracing::info!(actions = ?action_names, "running configured actions");
    let mut fingerprint = SweepFingerprint::default();

    for action in &action_names {
        match *action {
            "lint" => {
                let auto = daemon_config.is_enabled("lint");
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
                            let files: Vec<String> = report
                                .violations
                                .iter()
                                .map(|v| v.path.to_string_lossy().to_string())
                                .collect();
                            fingerprint.add("lint", files);
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
                let auto = daemon_config.is_enabled("link");
                let opts = crate::cli::LinkOpts {
                    apply: auto,
                    scan: "all".to_string(),
                };
                match crate::run_link(vault_root, config, &opts) {
                    Ok(report) => {
                        let count = report.violations.len();
                        if auto && count > 0 {
                            let files: Vec<String> = report
                                .violations
                                .iter()
                                .map(|v| v.path.to_string_lossy().to_string())
                                .collect();
                            fingerprint.add("link", files);
                            tracing::info!(fixes = count, "auto-applied link");
                            println!("[daemon] auto-applied link: {count} fix(es)");
                        } else if !report.is_empty() {
                            println!("[daemon] link: {count} suggestion(s)");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "link action failed"),
                }
            }
            "duplicates" => {
                let auto = daemon_config.is_enabled("duplicates");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::duplicates::apply_duplicates(vault_root, &notes, &config.actions.duplicates) {
                                Ok(count) if count > 0 => {
                                    let report = crate::duplicates::lint_duplicates(&notes, &config.actions.duplicates);
                                    let files: Vec<String> = report
                                        .violations
                                        .iter()
                                        .map(|v| v.path.to_string_lossy().to_string())
                                        .collect();
                                    fingerprint.add("duplicates", files);
                                    tracing::info!(fixes = count, "auto-applied duplicates");
                                    println!("[daemon] auto-applied duplicates: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => tracing::error!(error = %e, "duplicates apply failed"),
                            }
                        } else {
                            let report = crate::duplicates::lint_duplicates(&notes, &config.actions.duplicates);
                            if !report.is_empty() {
                                println!("[daemon] duplicates: {} violation(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "failed to scan vault for duplicates"),
                }
            }
            "auto-tag" => {
                let auto = daemon_config.is_enabled("auto-tag");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::autotag::apply_autotag(vault_root, &notes, &notes, &config.actions.auto_tag) {
                                Ok(count) if count > 0 => {
                                    let report = crate::autotag::lint_autotag(&notes, &notes, &config.actions.auto_tag);
                                    let files: Vec<String> = report
                                        .violations
                                        .iter()
                                        .map(|v| v.path.to_string_lossy().to_string())
                                        .collect();
                                    fingerprint.add("auto-tag", files);
                                    tracing::info!(fixes = count, "auto-applied auto-tag");
                                    println!("[daemon] auto-applied auto-tag: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => tracing::error!(error = %e, "auto-tag apply failed"),
                            }
                        } else {
                            let report = crate::autotag::lint_autotag(&notes, &notes, &config.actions.auto_tag);
                            if !report.is_empty() {
                                println!("[daemon] auto-tag: {} suggestion(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "failed to scan vault for auto-tag"),
                }
            }
            "quality" => {
                let auto = daemon_config.is_enabled("quality");
                match crate::vault::scan_vault(vault_root, &config.vault) {
                    Ok(notes) => {
                        if auto {
                            match crate::quality::apply_quality(vault_root, &notes, &config.actions.quality) {
                                Ok(count) if count > 0 => {
                                    let report = crate::quality::lint_quality(&notes, &config.actions.quality);
                                    let files: Vec<String> = report
                                        .violations
                                        .iter()
                                        .map(|v| v.path.to_string_lossy().to_string())
                                        .collect();
                                    fingerprint.add("quality", files);
                                    tracing::info!(fixes = count, "auto-applied quality");
                                    println!("[daemon] auto-applied quality: {count} fix(es)");
                                }
                                Ok(_) => {}
                                Err(e) => tracing::error!(error = %e, "quality apply failed"),
                            }
                        } else {
                            let report = crate::quality::lint_quality(&notes, &config.actions.quality);
                            if !report.is_empty() {
                                println!("[daemon] quality: {} violation(s)", report.violations.len());
                            }
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "failed to scan vault for quality"),
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
    fingerprint
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
    std::fs::write(&service_path, &service)?;
    println!("Installed: {}", service_path.display());

    // Daily intel timer - runs at 23:00 every day
    let daily_service = format!(
        "[Unit]\n\
         Description=Obsidian Cortex Daily Intel\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={binary}{config_flag} --vault {vault} intel --daily\n",
        binary = binary.display(),
    );

    let daily_timer = "[Unit]\n\
         Description=Obsidian Cortex Daily Intel Timer\n\
         \n\
         [Timer]\n\
         OnCalendar=*-*-* 23:00:00\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n";

    let daily_svc_path = service_dir.join("obsidian-cortex-daily.service");
    let daily_timer_path = service_dir.join("obsidian-cortex-daily.timer");
    std::fs::write(&daily_svc_path, daily_service)?;
    std::fs::write(&daily_timer_path, daily_timer)?;
    println!("Installed: {}", daily_svc_path.display());
    println!("Installed: {}", daily_timer_path.display());

    // Weekly intel timer - runs Sunday at 22:00
    let weekly_service = format!(
        "[Unit]\n\
         Description=Obsidian Cortex Weekly Intel\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={binary}{config_flag} --vault {vault} intel --weekly\n",
        binary = binary.display(),
    );

    let weekly_timer = "[Unit]\n\
         Description=Obsidian Cortex Weekly Intel Timer\n\
         \n\
         [Timer]\n\
         OnCalendar=Sun *-*-* 22:00:00\n\
         Persistent=true\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n";

    let weekly_svc_path = service_dir.join("obsidian-cortex-weekly.service");
    let weekly_timer_path = service_dir.join("obsidian-cortex-weekly.timer");
    std::fs::write(&weekly_svc_path, weekly_service)?;
    std::fs::write(&weekly_timer_path, weekly_timer)?;
    println!("Installed: {}", weekly_svc_path.display());
    println!("Installed: {}", weekly_timer_path.display());

    println!("\nRun:");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now obsidian-cortex");
    println!("  systemctl --user enable --now obsidian-cortex-daily.timer");
    println!("  systemctl --user enable --now obsidian-cortex-weekly.timer");

    Ok(())
}

/// Uninstall the systemd user service and timer units.
fn uninstall_systemd_service() -> Result<()> {
    let service_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("systemd")
        .join("user");

    let units = [
        "obsidian-cortex.service",
        "obsidian-cortex-daily.service",
        "obsidian-cortex-daily.timer",
        "obsidian-cortex-weekly.service",
        "obsidian-cortex-weekly.timer",
    ];

    let mut removed = false;
    for unit in &units {
        let path = service_dir.join(unit);
        if path.exists() {
            std::fs::remove_file(&path)?;
            println!("Removed: {}", path.display());
            removed = true;
        }
    }

    if removed {
        println!("Run: systemctl --user daemon-reload");
    } else {
        println!("No service files found");
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

/// Compute Duration until next occurrence of "HH:MM" today or tomorrow.
pub fn duration_until_daily(time_str: &str) -> Duration {
    let now = chrono::Local::now();
    let parts: Vec<&str> = time_str.split(':').collect();
    let hour: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(23);
    let minute: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    let today_target = now.date_naive().and_hms_opt(hour, minute, 0).expect("valid time");
    let today_target = today_target
        .and_local_timezone(chrono::Local)
        .single()
        .expect("valid local time");

    if today_target > now {
        (today_target - now).to_std().unwrap_or(Duration::from_secs(3600))
    } else {
        let tomorrow_target = today_target + chrono::Duration::days(1);
        (tomorrow_target - now).to_std().unwrap_or(Duration::from_secs(3600))
    }
}

/// Compute Duration until next occurrence of "Day HH:MM" (e.g., "Sun 22:00").
pub fn duration_until_weekly(schedule_str: &str) -> Duration {
    let now = chrono::Local::now();
    let parts: Vec<&str> = schedule_str.split_whitespace().collect();
    let day_str = parts.first().copied().unwrap_or("Sun");
    let time_str = parts.get(1).copied().unwrap_or("22:00");

    let target_weekday = match day_str.to_lowercase().as_str() {
        "mon" => chrono::Weekday::Mon,
        "tue" => chrono::Weekday::Tue,
        "wed" => chrono::Weekday::Wed,
        "thu" => chrono::Weekday::Thu,
        "fri" => chrono::Weekday::Fri,
        "sat" => chrono::Weekday::Sat,
        _ => chrono::Weekday::Sun,
    };

    let time_parts: Vec<&str> = time_str.split(':').collect();
    let hour: u32 = time_parts.first().and_then(|s| s.parse().ok()).unwrap_or(22);
    let minute: u32 = time_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

    let current_weekday = now.weekday();
    let days_ahead =
        (target_weekday.num_days_from_monday() as i64 - current_weekday.num_days_from_monday() as i64 + 7) % 7;

    let target_date = now.date_naive() + chrono::Duration::days(days_ahead);
    let target_time = target_date.and_hms_opt(hour, minute, 0).expect("valid time");
    let target = target_time
        .and_local_timezone(chrono::Local)
        .single()
        .expect("valid local time");

    if target > now {
        (target - now).to_std().unwrap_or(Duration::from_secs(3600))
    } else {
        // Same day but time already passed - next week
        let next_week = target + chrono::Duration::weeks(1);
        (next_week - now).to_std().unwrap_or(Duration::from_secs(3600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;

    #[test]
    fn test_is_enabled_default_is_false() {
        let config = DaemonConfig::default();
        assert!(!config.is_enabled("lint"));
        assert!(!config.is_enabled("link"));
        assert!(!config.is_enabled("nonexistent"));
    }

    #[test]
    fn test_is_enabled_explicit_true() {
        let mut config = DaemonConfig::default();
        config
            .actions
            .insert("lint".to_string(), crate::config::DaemonAction { enable: true });
        assert!(config.is_enabled("lint"));
        assert!(!config.is_enabled("link"));
    }

    #[test]
    fn test_is_enabled_explicit_false() {
        let config = DaemonConfig::default();
        // lint is in default actions but enable defaults to false
        assert!(!config.is_enabled("lint"));
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
            "actions:\n  lint:\n    enable: true\n  broken-links: {}\n  link:\n    enable: false\ndebounce-secs: 10\n";
        let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.debounce_secs, 10);
        assert!(config.is_enabled("lint"));
        assert!(!config.is_enabled("broken-links"));
        assert!(!config.is_enabled("link"));
        assert!(!config.is_enabled("nonexistent"));
        assert_eq!(config.actions.len(), 3);
    }

    #[test]
    fn test_sweep_fingerprint_empty_default() {
        let fp = SweepFingerprint::default();
        assert!(fp.is_empty());
    }

    #[test]
    fn test_sweep_fingerprint_non_empty() {
        let mut fp = SweepFingerprint::default();
        fp.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);
        assert!(!fp.is_empty());
    }

    #[test]
    fn test_sweep_fingerprint_equality() {
        let mut fp1 = SweepFingerprint::default();
        fp1.add("lint", vec!["b.md".to_string(), "a.md".to_string()]);

        let mut fp2 = SweepFingerprint::default();
        fp2.add("lint", vec!["a.md".to_string(), "b.md".to_string()]);

        // Both should sort to the same order
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_sweep_fingerprint_different_files() {
        let mut fp1 = SweepFingerprint::default();
        fp1.add("lint", vec!["a.md".to_string()]);

        let mut fp2 = SweepFingerprint::default();
        fp2.add("lint", vec!["b.md".to_string()]);

        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_sweep_fingerprint_empty_files_ignored() {
        let mut fp = SweepFingerprint::default();
        fp.add("lint", vec![]);
        assert!(fp.is_empty());
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

    #[test]
    fn test_duration_until_daily_future_today() {
        // If we ask for a time that hasn't passed yet today, it should be today
        let now = chrono::Local::now();
        let future_hour = (now.format("%H").to_string().parse::<u32>().unwrap_or(0) + 1) % 24;
        let time_str = format!("{future_hour:02}:00");
        let dur = duration_until_daily(&time_str);
        // Should be less than 24 hours
        assert!(dur < Duration::from_secs(24 * 3600));
        assert!(dur > Duration::ZERO);
    }

    #[test]
    fn test_duration_until_daily_already_passed() {
        // If we ask for a time that already passed, it should be tomorrow
        let now = chrono::Local::now();
        let past_hour = if now.format("%H").to_string().parse::<u32>().unwrap_or(0) > 0 {
            now.format("%H").to_string().parse::<u32>().unwrap_or(0) - 1
        } else {
            23
        };
        let time_str = format!("{past_hour:02}:00");
        let dur = duration_until_daily(&time_str);
        // Should be between ~23 hours and ~24 hours from now
        assert!(dur > Duration::from_secs(22 * 3600));
        assert!(dur <= Duration::from_secs(24 * 3600));
    }

    #[test]
    fn test_duration_until_weekly_returns_valid_duration() {
        let dur = duration_until_weekly("Sun 22:00");
        // Should be within 7 days
        assert!(dur <= Duration::from_secs(7 * 24 * 3600));
        assert!(dur > Duration::ZERO);
    }

    #[test]
    fn test_duration_until_weekly_all_days() {
        for day in &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] {
            let schedule = format!("{day} 12:00");
            let dur = duration_until_weekly(&schedule);
            assert!(dur <= Duration::from_secs(7 * 24 * 3600), "failed for {day}");
            assert!(dur > Duration::ZERO, "failed for {day}");
        }
    }

    #[test]
    fn test_daemon_config_deserialize_schedule_fields() {
        let yaml = "daily-at: \"23:00\"\nweekly-on: \"Sun 22:00\"\n";
        let config: DaemonConfig = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(config.daily_at.as_deref(), Some("23:00"));
        assert_eq!(config.weekly_on.as_deref(), Some("Sun 22:00"));
    }

    #[test]
    fn test_daemon_config_default_no_schedule() {
        let config = DaemonConfig::default();
        assert!(config.daily_at.is_none());
        assert!(config.weekly_on.is_none());
    }
}
