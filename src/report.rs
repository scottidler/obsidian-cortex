use colored::Colorize;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "ERROR"),
            Severity::Warning => write!(f, "WARN"),
            Severity::Info => write!(f, "INFO"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum Fix {
    RenameFile { from: PathBuf, to: PathBuf },
    SetFrontmatter { key: String, value: serde_yaml::Value },
    ReplaceTag { old: String, new: String },
    AddWikilink { target: String, context: String },
    MoveFile { from: PathBuf, to: PathBuf },
    SetCortexFields { fields: Vec<(String, String)> },
    RemoveCortexFields { keys: Vec<String> },
}

#[derive(Debug, Clone, Serialize)]
pub struct Violation {
    pub path: PathBuf,
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub violations: Vec<Violation>,
}

impl Report {
    pub fn add(&mut self, violation: Violation) {
        self.violations.push(violation);
    }

    pub fn merge(&mut self, other: Report) {
        self.violations.extend(other.violations);
    }

    pub fn error_count(&self) -> usize {
        self.violations.iter().filter(|v| v.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.violations
            .iter()
            .filter(|v| v.severity == Severity::Warning)
            .count()
    }

    pub fn info_count(&self) -> usize {
        self.violations.iter().filter(|v| v.severity == Severity::Info).count()
    }

    pub fn is_empty(&self) -> bool {
        self.violations.is_empty()
    }

    /// Print report in human-readable format.
    pub fn print_human(&self) {
        if self.is_empty() {
            println!("{}", "No violations found.".green());
            return;
        }

        for v in &self.violations {
            let severity_str = match v.severity {
                Severity::Error => format!("{}", v.severity).red().bold(),
                Severity::Warning => format!("{}", v.severity).yellow(),
                Severity::Info => format!("{}", v.severity).blue(),
            };
            println!("{} [{}] {}: {}", severity_str, v.rule, v.path.display(), v.message);
            if let Some(ref fix) = v.fix {
                let fix_desc = match fix {
                    Fix::RenameFile { from, to } => {
                        format!("rename {} -> {}", from.display(), to.display())
                    }
                    Fix::SetFrontmatter { key, value } => {
                        format!("set {key}: {value:?}")
                    }
                    Fix::ReplaceTag { old, new } => format!("replace tag {old} -> {new}"),
                    Fix::AddWikilink { target, .. } => format!("add link [[{target}]]"),
                    Fix::MoveFile { from, to } => {
                        format!("move {} -> {}", from.display(), to.display())
                    }
                    Fix::SetCortexFields { fields } => {
                        let pairs: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        format!("set {}", pairs.join(", "))
                    }
                    Fix::RemoveCortexFields { keys } => {
                        format!("remove {}", keys.join(", "))
                    }
                };
                println!("  {} {}", "fix:".dimmed(), fix_desc.dimmed());
            }
        }

        println!();
        println!(
            "{}",
            format!(
                "Total: {} error(s), {} warning(s), {} info(s)",
                self.error_count(),
                self.warning_count(),
                self.info_count()
            )
            .bold()
        );
    }

    /// Print report as JSON.
    pub fn print_json(&self) -> eyre::Result<()> {
        let json = serde_json::to_string_pretty(&self.violations)?;
        println!("{json}");
        Ok(())
    }
}
