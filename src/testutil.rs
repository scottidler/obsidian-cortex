//! Shared test utilities for creating isolated mini-vaults.
//!
//! Every test that needs a vault gets its own `TestVault` in a fresh tmpdir.
//! The fixture creates a realistic set of notes covering all the cases our
//! modules need to validate: good notes, bad names, missing frontmatter,
//! alias tags, broken links, duplicates, scope-tagged notes, etc.

#![cfg(test)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{
    ActionsConfig, BrokenLinksConfig, Config, DaemonConfig, DuplicatesConfig, FrontmatterConfig, IntelConfig,
    LinkingConfig, LinkingEntities, LlmConfig, NamingConfig, ScopeConfig, ScopeMatch, ScopeRule, StateConfig,
    TagsConfig, VaultConfig,
};
use crate::vault::{self, Frontmatter, Note};

/// An isolated mini-vault in a temp directory.
/// Dropped automatically when it goes out of scope.
pub struct TestVault {
    pub dir: tempfile::TempDir,
}

impl Default for TestVault {
    fn default() -> Self {
        Self::new()
    }
}

impl TestVault {
    /// Create a fresh mini-vault with a realistic set of notes.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let root = dir.path();

        // -- Well-formed notes --
        write(
            root,
            "rust-guide.md",
            "---\ntitle: Rust Guide\ndate: 2026-03-10\ntype: note\ntags:\n  - rust\n  - programming\n---\nA guide to Rust programming.\n\nSee also the Python Guide for comparisons.\n",
        );
        write(
            root,
            "python-guide.md",
            "---\ntitle: Python Guide\ndate: 2026-03-11\ntype: note\ntags:\n  - python\n  - programming\n---\nA guide to Python programming.\n\nRelated: [[rust-guide]]\n",
        );
        write(
            root,
            "daily-standup.md",
            "---\ntitle: Daily Standup\ndate: 2026-03-16\ntype: meeting\ntags:\n  - sre\n  - tatari\n---\nDiscussed deployment pipeline.\n\nJohn Smith presented the new approach.\n",
        );

        // -- Bad filename (not lowercase-hyphenated) --
        write(
            root,
            "My Awesome Note.md",
            "---\ntitle: My Awesome Note\ndate: 2026-03-12\ntype: note\ntags:\n  - writing\n---\nThis filename violates naming conventions.\n",
        );

        // -- Missing frontmatter entirely --
        write(root, "bare-note.md", "Just some text, no frontmatter at all.\n");

        // -- Missing required fields (no date, no type, no tags) --
        write(
            root,
            "partial-frontmatter.md",
            "---\ntitle: Partial\n---\nHas title but missing date, type, tags.\n",
        );

        // -- Tag that is an alias (should resolve) --
        write(
            root,
            "ai-research.md",
            "---\ntitle: AI Research\ndate: 2026-03-13\ntype: research\ntags:\n  - ai\n  - k8s\n---\nResearch on AI and Kubernetes.\n",
        );

        // -- Non-canonical tag --
        write(
            root,
            "hobby-project.md",
            "---\ntitle: Hobby Project\ndate: 2026-03-14\ntype: note\ntags:\n  - obscure-hobby\n---\nA personal hobby project.\n",
        );

        // -- Broken wikilink --
        write(
            root,
            "linker.md",
            "---\ntitle: Linker\ndate: 2026-03-15\ntype: note\ntags:\n  - rust\n---\nSee [[nonexistent-page]] for more.\n\nAlso see [[rust-guide]] which exists.\n",
        );

        // -- Duplicate content (exact) --
        write(
            root,
            "duplicate-a.md",
            "---\ntitle: Duplicate A\ndate: 2026-03-14\ntype: note\ntags:\n  - rust\n---\nThis is the exact same body content for duplicate detection.\n",
        );
        write(
            root,
            "duplicate-b.md",
            "---\ntitle: Duplicate B\ndate: 2026-03-14\ntype: note\ntags:\n  - rust\n---\nThis is the exact same body content for duplicate detection.\n",
        );

        // -- Scope: work note with granola source --
        write(
            root,
            "work-meeting.md",
            "---\ntitle: Work Meeting\ndate: 2026-03-16\ntype: meeting\ntags:\n  - sre\nsource: granola-meeting-notes\n---\nConfidential work meeting.\n",
        );

        // -- Video type (missing type-specific fields: source, channel) --
        write(
            root,
            "cool-video.md",
            "---\ntitle: Cool Video\ndate: 2026-03-15\ntype: video\ntags:\n  - rust\n---\nNotes on a cool video.\n",
        );

        // -- Note in a subfolder --
        write(
            root,
            "projects/obsidian-cortex.md",
            "---\ntitle: Obsidian Cortex\ndate: 2026-03-16\ntype: note\ntags:\n  - rust\n  - obsidian\n---\nThe vault governance tool.\n",
        );

        // -- Protected file (should be skipped) --
        write(
            root,
            "system/borg-ledger.md",
            "---\ntitle: Borg Ledger\n---\nManaged by borg, do not touch.\n",
        );

        // -- Ignored directory (.obsidian) --
        write(
            root,
            ".obsidian/workspace.md",
            "---\ntitle: Workspace\n---\nObsidian internal.\n",
        );

        // -- Non-markdown file (should be ignored) --
        fs::write(root.join("readme.txt"), "Not a note.").expect("write txt");

        TestVault { dir }
    }

    /// Path to the vault root.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Parse all notes in the vault using the standard scanner.
    pub fn scan(&self) -> Vec<Note> {
        vault::scan_vault(self.root(), &self.vault_config()).expect("scan vault")
    }

    /// Parse all notes with a custom VaultConfig.
    pub fn scan_with(&self, config: &VaultConfig) -> Vec<Note> {
        vault::scan_vault(self.root(), config).expect("scan vault")
    }

    /// Return the default VaultConfig matching the fixture layout.
    pub fn vault_config(&self) -> VaultConfig {
        VaultConfig {
            root_path: None,
            ignore: vec![".git".to_string(), ".obsidian".to_string()],
            protected: vec!["system/borg-ledger.md".to_string()],
        }
    }

    /// Return a full Config wired to this vault.
    pub fn config(&self) -> Config {
        Config {
            vault: self.vault_config(),
            log_level: "warn".to_string(),
            actions: ActionsConfig {
                naming: NamingConfig {
                    style: "lowercase-hyphenated".to_string(),
                    max_length: 80,
                    exempt_patterns: Vec::new(),
                },
                frontmatter: FrontmatterConfig {
                    required: vec![
                        "title".to_string(),
                        "date".to_string(),
                        "type".to_string(),
                        "tags".to_string(),
                    ],
                    type_fields: {
                        let mut m = HashMap::new();
                        m.insert("video".to_string(), vec!["source".to_string(), "channel".to_string()]);
                        m.insert("meeting".to_string(), vec!["scope".to_string(), "company".to_string()]);
                        m
                    },
                    auto_title: true,
                },
                tags: TagsConfig {
                    style: "lowercase-hyphenated".to_string(),
                    canonical: vec![
                        "rust".to_string(),
                        "python".to_string(),
                        "programming".to_string(),
                        "ai-llm".to_string(),
                        "kubernetes".to_string(),
                        "sre".to_string(),
                        "obsidian".to_string(),
                        "writing".to_string(),
                    ],
                    aliases: {
                        let mut m = HashMap::new();
                        m.insert("ai".to_string(), "ai-llm".to_string());
                        m.insert("k8s".to_string(), "kubernetes".to_string());
                        m
                    },
                },
                scope: ScopeConfig {
                    rules: vec![
                        ScopeRule {
                            match_criteria: ScopeMatch {
                                tags: Some(vec!["sre".to_string(), "tatari".to_string()]),
                                source_contains: None,
                            },
                            set: {
                                let mut m = HashMap::new();
                                m.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));
                                m.insert("company".to_string(), serde_yaml::Value::String("tatari".to_string()));
                                m
                            },
                        },
                        ScopeRule {
                            match_criteria: ScopeMatch {
                                tags: None,
                                source_contains: Some("granola".to_string()),
                            },
                            set: {
                                let mut m = HashMap::new();
                                m.insert("scope".to_string(), serde_yaml::Value::String("work".to_string()));
                                m.insert("confidential".to_string(), serde_yaml::Value::Bool(true));
                                m
                            },
                        },
                    ],
                },
                linking: LinkingConfig {
                    scan_for: vec!["all".to_string()],
                    entities: LinkingEntities {
                        people: vec!["John Smith".to_string()],
                        projects: vec!["obsidian-cortex".to_string()],
                    },
                },
                intel: IntelConfig {
                    daily_note: true,
                    weekly_review: true,
                    fabric_patterns: vec![],
                    output_path: "ai-output".to_string(),
                },
                duplicates: DuplicatesConfig {
                    threshold: 0.85,
                    same_type_only: false,
                },
                broken_links: BrokenLinksConfig {
                    check_wikilinks: true,
                    check_urls: false,
                },
            },
            state: StateConfig {
                cache_dir: ".cortex".to_string(),
            },
            daemon: DaemonConfig::default(),
            migrations: Vec::new(),
            llm: LlmConfig::default(),
        }
    }

    /// Write an additional note into the vault (for test-specific needs).
    pub fn add_note(&self, relative_path: &str, content: &str) {
        write(self.root(), relative_path, content);
    }

    /// Read a file from the vault.
    pub fn read(&self, relative_path: &str) -> String {
        fs::read_to_string(self.root().join(relative_path)).expect("read file")
    }

    /// Check if a file exists in the vault.
    pub fn exists(&self, relative_path: &str) -> bool {
        self.root().join(relative_path).exists()
    }
}

/// Write a file into the vault, creating parent directories as needed.
fn write(root: &Path, relative_path: &str, content: &str) {
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(&path, content).expect("write file");
}

/// Build a Note struct in-memory (no filesystem). Useful for pure-logic tests.
pub struct NoteBuilder {
    path: PathBuf,
    title: Option<String>,
    date: Option<String>,
    note_type: Option<String>,
    tags: Option<Vec<String>>,
    extra: HashMap<String, serde_yaml::Value>,
    body: String,
    raw: String,
}

impl NoteBuilder {
    pub fn new(path: &str) -> Self {
        Self {
            path: PathBuf::from(path),
            title: None,
            date: None,
            note_type: None,
            tags: None,
            extra: HashMap::new(),
            body: String::new(),
            raw: String::new(),
        }
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    pub fn date(mut self, date: &str) -> Self {
        self.date = Some(date.to_string());
        self
    }

    pub fn note_type(mut self, note_type: &str) -> Self {
        self.note_type = Some(note_type.to_string());
        self
    }

    pub fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = Some(tags.iter().map(|s| s.to_string()).collect());
        self
    }

    pub fn extra(mut self, key: &str, value: serde_yaml::Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    pub fn body(mut self, body: &str) -> Self {
        self.body = body.to_string();
        self
    }

    pub fn raw(mut self, raw: &str) -> Self {
        self.raw = raw.to_string();
        self
    }

    pub fn build(self) -> Note {
        Note {
            path: self.path,
            frontmatter: Frontmatter {
                title: self.title,
                date: self.date,
                note_type: self.note_type,
                tags: self.tags,
                extra: self.extra,
            },
            body: self.body,
            raw: self.raw,
        }
    }
}
