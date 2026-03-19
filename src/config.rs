use eyre::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub vault: VaultConfig,
    #[serde(rename = "log-level")]
    pub log_level: String,
    pub schema: SchemaConfig,
    pub actions: ActionsConfig,
    pub state: StateConfig,
    pub daemon: DaemonConfig,
    pub migrations: Vec<MigrationConfig>,
    pub llm: LlmConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            vault: VaultConfig::default(),
            log_level: "info".to_string(),
            schema: SchemaConfig::default(),
            actions: ActionsConfig::default(),
            state: StateConfig::default(),
            daemon: DaemonConfig::default(),
            migrations: Vec::new(),
            llm: LlmConfig::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SchemaConfig {
    pub domains: Vec<String>,
    pub types: Vec<String>,
    pub origins: Vec<String>,
    pub statuses: Vec<String>,
    pub methods: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    #[serde(rename = "root-path")]
    pub root_path: Option<String>,
    pub ignore: Vec<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            root_path: None,
            ignore: vec![
                ".git".to_string(),
                ".obsidian".to_string(),
                ".cortex".to_string(),
                "assets".to_string(),
                "attachments".to_string(),
            ],
            exclude: Vec::new(),
            include: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ActionsConfig {
    pub naming: NamingConfig,
    pub frontmatter: FrontmatterConfig,
    pub tags: TagsConfig,
    pub scope: ScopeConfig,
    pub linking: LinkingConfig,
    pub intel: IntelConfig,
    pub duplicates: DuplicatesConfig,
    #[serde(rename = "broken-links")]
    pub broken_links: BrokenLinksConfig,
    pub quality: QualityConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct NamingConfig {
    pub style: String,
    #[serde(rename = "max-length")]
    pub max_length: u32,
    #[serde(rename = "exempt-patterns")]
    pub exempt_patterns: Vec<String>,
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            style: "lowercase-hyphenated".to_string(),
            max_length: 80,
            exempt_patterns: vec![r"^[\p{Emoji}].*/$".to_string()],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FrontmatterConfig {
    pub required: Vec<String>,
    pub exempt: HashMap<String, Vec<String>>,
    #[serde(rename = "path-exempt")]
    pub path_exempt: HashMap<String, Vec<String>>,
    #[serde(rename = "type-fields")]
    pub type_fields: HashMap<String, Vec<String>>,
    #[serde(rename = "auto-title")]
    pub auto_title: bool,
}

impl Default for FrontmatterConfig {
    fn default() -> Self {
        Self {
            required: vec![
                "title".to_string(),
                "date".to_string(),
                "type".to_string(),
                "tags".to_string(),
            ],
            exempt: HashMap::new(),
            path_exempt: HashMap::new(),
            type_fields: HashMap::new(),
            auto_title: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct TagsConfig {
    pub style: String,
    pub canonical: Vec<String>,
    pub aliases: HashMap<String, String>,
}

impl Default for TagsConfig {
    fn default() -> Self {
        Self {
            style: "lowercase-hyphenated".to_string(),
            canonical: Vec::new(),
            aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ScopeConfig {
    pub rules: Vec<ScopeRule>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeRule {
    #[serde(rename = "match")]
    pub match_criteria: ScopeMatch,
    pub set: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ScopeMatch {
    pub tags: Option<Vec<String>>,
    #[serde(rename = "source-contains")]
    pub source_contains: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LinkingConfig {
    #[serde(rename = "scan-for")]
    pub scan_for: Vec<String>,
    pub entities: LinkingEntities,
    pub targets: LinkingTargets,
    #[serde(rename = "min-word-length")]
    pub min_word_length: usize,
}

impl Default for LinkingConfig {
    fn default() -> Self {
        Self {
            scan_for: vec!["people".to_string(), "projects".to_string(), "concepts".to_string()],
            entities: LinkingEntities::default(),
            targets: LinkingTargets::default(),
            min_word_length: 5,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingEntities {
    pub people: Vec<String>,
    pub projects: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingTargets {
    pub types: LinkingFilter,
    pub paths: LinkingFilter,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LinkingFilter {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct IntelConfig {
    #[serde(rename = "daily-note")]
    pub daily_note: bool,
    #[serde(rename = "weekly-review")]
    pub weekly_review: bool,
    #[serde(rename = "fabric-patterns")]
    pub fabric_patterns: Vec<String>,
    #[serde(rename = "output-path")]
    pub output_path: String,
}

impl Default for IntelConfig {
    fn default() -> Self {
        Self {
            daily_note: true,
            weekly_review: true,
            fabric_patterns: vec!["extract_wisdom".to_string(), "summarize".to_string()],
            output_path: "system/ai-output".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DuplicatesConfig {
    pub threshold: f64,
    #[serde(rename = "same-type-only")]
    pub same_type_only: bool,
}

impl Default for DuplicatesConfig {
    fn default() -> Self {
        Self {
            threshold: 0.85,
            same_type_only: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct QualityConfig {
    #[serde(rename = "min-word-count")]
    pub min_word_count: usize,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self { min_word_count: 50 }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct BrokenLinksConfig {
    #[serde(rename = "check-wikilinks")]
    pub check_wikilinks: bool,
    #[serde(rename = "check-urls")]
    pub check_urls: bool,
}

impl Default for BrokenLinksConfig {
    fn default() -> Self {
        Self {
            check_wikilinks: true,
            check_urls: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    #[serde(rename = "cache-dir")]
    pub cache_dir: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            cache_dir: ".cortex".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub actions: HashMap<String, DaemonAction>,
    #[serde(rename = "debounce-secs")]
    pub debounce_secs: u64,
    pub watch: String,
    #[serde(rename = "poll-interval")]
    pub poll_interval: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DaemonAction {
    pub apply: bool,
}

impl DaemonConfig {
    /// Get the list of enabled action names.
    pub fn enabled_actions(&self) -> Vec<&str> {
        self.actions.keys().map(|s| s.as_str()).collect()
    }

    /// Check whether a given action should auto-apply.
    pub fn should_apply(&self, action: &str) -> bool {
        self.actions.get(action).is_some_and(|a| a.apply)
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let mut actions = HashMap::new();
        actions.insert("lint".to_string(), DaemonAction { apply: false });
        actions.insert("broken-links".to_string(), DaemonAction { apply: false });
        Self {
            actions,
            debounce_secs: 5,
            watch: "notify".to_string(),
            poll_interval: 300,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct MigrationConfig {
    pub name: String,
    #[serde(default)]
    pub moves: Vec<MigrationMove>,
    #[serde(rename = "field-renames", default)]
    pub field_renames: HashMap<String, String>,
    #[serde(rename = "field-drops", default)]
    pub field_drops: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct MigrationMove {
    pub from: String,
    pub to: String,
    #[serde(rename = "set-frontmatter")]
    pub set_frontmatter: Option<HashMap<String, serde_yaml::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    #[serde(rename = "api-key")]
    pub api_key: String,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            api_key: "ANTHROPIC_API_KEY".to_string(),
        }
    }
}

impl Config {
    /// Load configuration with fallback chain:
    /// 1. Explicit --config flag
    /// 2. ~/.config/obsidian-cortex/obsidian-cortex.yml
    /// 3. Defaults
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        if let Some(path) = config_path {
            return Self::load_from_file(path).context(format!("Failed to load config from {}", path.display()));
        }

        if let Some(config_dir) = dirs::config_dir() {
            let primary = config_dir.join("obsidian-cortex").join("obsidian-cortex.yml");
            if primary.exists() {
                match Self::load_from_file(&primary) {
                    Ok(config) => return Ok(config),
                    Err(e) => {
                        tracing::warn!(
                            path = %primary.display(),
                            error = %e,
                            "failed to load config, falling back to defaults"
                        );
                    }
                }
            }
        }

        tracing::info!("no config file found, using defaults");
        Ok(Self::default())
    }

    fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref()).context("failed to read config file")?;
        let config: Self = serde_yaml::from_str(&content).context("failed to parse config file")?;
        tracing::info!(path = %path.as_ref().display(), "loaded config");
        Ok(config)
    }

    /// Resolve the vault root path from CLI flag, config, or CWD.
    pub fn vault_root(&self, cli_vault: Option<&PathBuf>) -> PathBuf {
        if let Some(vault) = cli_vault {
            return vault.clone();
        }
        if let Some(ref root_path) = self.vault.root_path {
            let expanded = shellexpand::tilde(root_path);
            return PathBuf::from(expanded.as_ref());
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}
