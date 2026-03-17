# Design Document: obsidian-cortex

**Author:** Scott Idler
**Date:** 2026-03-16
**Status:** Draft
**Review Passes Completed:** 5/5

## Summary

obsidian-cortex is a Rust CLI and daemon that governs an Obsidian vault - enforcing naming conventions, validating frontmatter, normalizing tags, detecting broken links and duplicates, inferring wikilinks, generating intelligence (daily/weekly notes), and migrating vault structure between schemas. It is the governance and intelligence companion to obsidian-borg (ingestion). Borg assimilates; Cortex processes.

## Tool Ecosystem

```
                    ┌─────────────────────┐
   URLs, text,      │                     │    Summarized notes
   images, audio ──>│   obsidian-borg     │──> with frontmatter
                    │   (ingestion)       │    into vault
                    └─────────────────────┘
                              |
                              v
                    ┌─────────────────────┐
                    │                     │
                    │   Obsidian Vault    │<── Manual editing
                    │   (~1000 .md files) │    (Obsidian, Claude Code)
                    │                     │
                    └─────────────────────┘
                              |
                              v
                    ┌─────────────────────┐
                    │                     │    Violations, fixes,
                    │   obsidian-cortex   │──> digests, wikilinks,
                    │   (governance)      │    intelligence
                    └─────────────────────┘
```

Cortex can also feed Obsidian directly - its CLI output (JSON, structured markdown) can be consumed by Obsidian Templater scripts or custom JS functions that shell out to the cortex binary.

## Problem Statement

### Background

An Obsidian vault is fed by obsidian-borg (URL ingestion via Telegram, Discord, HTTP, clipboard, CLI) and by manual note creation via Claude Code and the Obsidian editor. Over time, entropy accumulates: inconsistent filenames, missing frontmatter fields, non-canonical tags, notes that should be linked but aren't, duplicates, and broken wikilinks.

Currently, governance is manual - Claude Code follows CLAUDE.md conventions when asked, and periodic "sweep the inbox" operations clean things up. But there's no automated enforcement, no change detection, and no intelligence layer that surfaces patterns across the vault.

### Problem

The vault has no automated governance. Standards drift. Notes that should be linked sit in isolation. Patterns across hundreds of notes go undetected. The human cost of manual maintenance scales linearly with vault size.

### Goals

- Enforce vault conventions automatically (naming, frontmatter, tags) via a config-driven rule engine
- Detect and report (or fix) structural problems: broken links, duplicates, missing fields
- Infer and create wikilinks between related notes (proper nouns, concepts, projects)
- Generate intelligence: daily digests, weekly reviews, pattern detection across the vault
- Provide fast change detection so governance runs only when needed
- Run as both a one-shot CLI and a long-lived daemon watching for changes
- Support config-driven vault structure migrations (folder reorganization, schema evolution)
- Produce structured output (human-readable and JSON) consumable by Obsidian templates and scripts

### Non-Goals

- Replacing obsidian-borg's ingestion pipeline - cortex does not fetch URLs or create notes from external sources
- Replacing Obsidian's own search/query features (Dataview, etc.) - cortex operates at the file level
- Auto-committing to git - git operations remain manual

## Proposed Solution

### Overview

A Rust binary with a thin `main.rs` (CLI parsing via clap derive, dispatch to lib) and a fat `lib.rs` (module declarations, orchestration functions). Each governance concern is a separate module. All behavior is driven by `~/.config/obsidian-cortex/obsidian-cortex.yml`. The CLI controls which actions run; the config defines how they behave.

**Vault root resolution:** If `--vault` is passed, use that path. Otherwise, use the `vault.root-path` from config. If neither is set, assume CWD is the vault root. This means cortex can be run from the vault directory with zero flags.

Two execution modes:
1. **CLI mode** - one-shot commands (`cortex lint`, `cortex link`, `cortex state`, `cortex intel`)
2. **Daemon mode** - watches for filesystem changes, runs actions when vault state changes

### Architecture

```
main.rs (thin)
  - Cli::parse() via clap derive
  - load_config() via config fallback chain
  - resolve log level: CLI --log-level > OBSIDIAN_CORTEX_LOG env var > config log-level > INFO
  - setup tracing subscriber
  - match on Command enum, dispatch to lib.rs functions

lib.rs (fat)
  - pub mod declarations for all modules
  - #[instrument] on all public orchestration functions
  - pub async fn run_lint(config, opts) -> Result<Report>
  - pub async fn run_link(config, opts) -> Result<Report>
  - pub async fn run_intel(config, opts) -> Result<()>
  - pub async fn run_state(config, opts) -> Result<()>
  - pub async fn run_daemon(config, opts) -> Result<()>
  - pub async fn run_migrate(config, opts) -> Result<Report>
```

#### Module Layout

```
src/
  main.rs          # CLI parsing, dispatch (thin)
  lib.rs           # Module declarations, orchestration functions (fat)
  cli.rs           # Clap structs: Cli, Command enum, subcommand opts
  config.rs        # Config loading (fallback chain), struct definitions
  state.rs         # Vault manifest, mtime-based change detection
  vault.rs         # Vault traversal, note parsing, frontmatter extraction
  naming.rs        # Filename convention enforcement (lowercase-hyphenated)
  frontmatter.rs   # Frontmatter validation, required fields by type
  tags.rs          # Tag normalization, canonical list, alias resolution
  linking.rs       # Wikilink inference, proper noun scan, concept matching
  intel.rs         # Daily/weekly note generation, pattern detection
  duplicates.rs    # Content similarity detection, dedup reporting
  broken_links.rs  # Wikilink target validation, dead link detection
  scope.rs         # Work/personal classification rules
  migrate.rs       # Schema evolution and vault structure migration
  daemon.rs        # Filesystem watcher, change-triggered action runner
  fabric.rs        # Fabric pattern integration for LLM-powered features
  logging.rs       # Tracing setup, log level resolution
  report.rs        # Structured output for lint/link results (human + JSON)
```

#### Instrumentation

All important functions are instrumented with `tracing` as the first concern. Parameters and key values are logged at the function boundary:

```rust
use tracing::instrument;

#[instrument(skip(config), fields(vault_root = %config.vault_root().display()))]
pub async fn run_lint(config: &Config, opts: &LintOpts) -> Result<Report> {
    tracing::info!("starting lint run");
    // ...
}

#[instrument(fields(path = %note_path.display()))]
fn validate_frontmatter(note_path: &Path, fm: &Frontmatter, rules: &FrontmatterConfig) -> Vec<Violation> {
    tracing::debug!(?fm.note_type, ?fm.tags, "validating frontmatter");
    // ...
}
```

#### Data Flow

```
                  obsidian-cortex.yml
                          |
                    [load_config]
                          |
              +-----------+-----------+
              |                       |
          CLI mode              Daemon mode
              |                       |
        [run_lint]              [fs watcher]
        [run_link]                    |
        [run_intel]             [on change]
        [run_state]                   |
              |               [run configured
              |                 actions]
              |                       |
              +----------+------------+
                         |
                  [vault.rs: scan]
                         |
               [parse each .md file]
                         |
        +----+----+----+----+----+----+
        |    |    |    |    |    |    |
      name  fm  tags  scope link dup broken
        |    |    |    |    |    |    |
        +----+----+----+----+----+----+
                         |
                  [report / fix]
```

### Data Model

#### VaultManifest (state.rs)

A simple manifest of file metadata for CLI-mode change detection. No cryptographic hashing of vault state.

```rust
/// Per-file metadata for change detection (no content read needed)
#[derive(Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub path: PathBuf,       // relative to vault root
    pub size: u64,
    pub mtime: u64,          // unix timestamp
}

/// Manifest of the entire vault from the last run
#[derive(Serialize, Deserialize)]
pub struct VaultManifest {
    pub timestamp: DateTime<Utc>,
    pub files: Vec<FileEntry>,
}
```

Manifest is cached to `<vault_root>/.cortex/manifest.yml` (always relative to vault root, never CWD).

**Change detection strategy:**
- **Daemon mode:** The `notify` crate delivers filesystem events directly - cortex knows exactly which files changed. No manifest comparison needed.
- **CLI mode:** On each run, walk the vault and compare (path, size, mtime) against the cached manifest. The diff tells you exactly which files were added, removed, or modified. Only those files need re-scanning. After the run, write the new manifest.
- **Content hashing:** SHA-256 of file contents is only computed on demand for duplicate detection. It is never used for change detection.

**Files without frontmatter:** If a `.md` file has no YAML frontmatter delimiters (`---`), cortex treats the entire file as body with an empty `Frontmatter` (all fields None). The frontmatter lint rule reports this as an Error-severity violation. With `--apply`, cortex inserts a minimal frontmatter block (title derived from filename, date from file mtime, type: note, tags: []).

#### Note (vault.rs)

```rust
/// Parsed representation of a vault note
pub struct Note {
    pub path: PathBuf,           // relative to vault root
    pub frontmatter: Frontmatter,
    pub body: String,            // everything after the closing ---
    pub raw: String,             // original file contents
}

/// Note: Do NOT use #[serde(flatten)] with serde_yaml - it has known
/// issues with YAML. Instead, parse frontmatter as serde_yaml::Value
/// first, extract known fields manually, and keep the rest as extras.
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub note_type: Option<String>,  // "type" in YAML
    pub tags: Option<Vec<String>>,
    pub extra: HashMap<String, serde_yaml::Value>,
}

impl Frontmatter {
    /// Parse from a serde_yaml::Value (typically a Mapping).
    /// Known fields are extracted; everything else goes into extra.
    pub fn from_value(value: serde_yaml::Value) -> Result<Self> { /* ... */ }

    /// Serialize back to YAML string, preserving extra fields.
    pub fn to_yaml(&self) -> Result<String> { /* ... */ }
}
```

#### Violation (report.rs)

```rust
pub enum Severity {
    Error,    // must fix: missing required frontmatter, invalid filename
    Warning,  // should fix: non-canonical tag, missing optional field
    Info,     // suggestion: potential wikilink, possible duplicate
}

pub struct Violation {
    pub path: PathBuf,
    pub rule: String,        // e.g. "naming.lowercase-hyphenated"
    pub severity: Severity,
    pub message: String,
    pub fix: Option<Fix>,    // auto-fixable?
}

pub enum Fix {
    RenameFile { from: PathBuf, to: PathBuf },
    SetFrontmatter { key: String, value: serde_yaml::Value },
    ReplaceTag { old: String, new: String },
    AddWikilink { target: String, context: String },
    MoveFile { from: PathBuf, to: PathBuf },  // for migrate
}
```

### API Design (CLI Interface)

```
obsidian-cortex [OPTIONS] <COMMAND>

Options:
  -c, --config <PATH>       Path to config file
  -V, --vault <PATH>        Vault root directory (default: CWD)
  -v, --verbose              Enable verbose output
  -l, --log-level <LEVEL>   Log level: trace, debug, info, warn, error
                             Resolution: --log-level > OBSIDIAN_CORTEX_LOG env > config > info

Commands:
  lint      Validate vault against rules
  link      Scan for and create wikilinks
  intel     Generate intelligence (daily/weekly notes)
  state     Vault state fingerprinting
  daemon    Watch mode - run actions on change
  migrate   Schema evolution and vault structure migration
```

`--help` output includes a tool dependency check and XDG log path, matching obsidian-borg's pattern:

```
REQUIRED TOOLS:
  ✅ fabric    1.4.100

Logs: ~/.local/share/obsidian-cortex/logs/obsidian-cortex.log
```

#### Subcommand Details

`cortex lint` runs all deterministic rules by default. Use `--rule` to run only specific rules:

| `--rule` value | Config section | Module |
|----------------|---------------|--------|
| `naming` | `actions.naming` | `naming.rs` |
| `frontmatter` | `actions.frontmatter` | `frontmatter.rs` |
| `tags` | `actions.tags` | `tags.rs` |
| `scope` | `actions.scope` | `scope.rs` |
| `broken-links` | `actions.broken-links` | `broken_links.rs` |

Without `--rule`, all rules run.

```
cortex lint [OPTIONS]
  --dry-run       Report violations without fixing (default)
  --apply         Auto-fix what's fixable
  --format <FMT>  Output format: human (default), json
  --rule <RULE>   Run only specific rule(s): naming, frontmatter, tags, scope, broken-links
  --path <PATH>   Lint only files matching glob pattern

cortex link [OPTIONS]
  --dry-run       Report suggested links without applying (default)
  --apply         Insert wikilinks into notes
  --scan <TYPE>   What to scan for: people, projects, concepts, all (default)

cortex intel [OPTIONS]
  --daily         Generate today's daily digest
  --weekly        Generate weekly review
  --output <PATH> Write to specific path (default: vault daily note)

cortex state [OPTIONS]
  --refresh       Recompute and cache vault manifest
  --diff          Show what changed since last cached manifest

cortex daemon [OPTIONS]
  --install       Install systemd user service
  --uninstall     Remove systemd user service
  --start         Start watching (used by systemd ExecStart)
  --stop          Stop watching
  --status        Show daemon status

cortex migrate [OPTIONS]
  --dry-run       Preview changes (default)
  --apply         Apply migrations
  --plan <PATH>   Path to migration plan YAML (default: migrations section in config)
```

### Configuration

Config file: `~/.config/obsidian-cortex/obsidian-cortex.yml`

Config fallback chain:
1. Explicit `--config` flag
2. `~/.config/obsidian-cortex/obsidian-cortex.yml`
3. Defaults

```yaml
vault:
  # Vault root path. If omitted, CWD is assumed.
  # root-path: ~/path/to/vault
  # Folders to skip during scanning
  ignore:
    - ".git"
    - ".obsidian"
    - ".cortex"
    - "assets"
    - "attachments"
  # Files managed by other tools - never lint or modify
  protected:
    - "⚙️ System/borg-ledger.md"
    - "⚙️ System/borg-dashboard.md"

# Log level: trace, debug, info, warn, error
# Resolution: --log-level flag > OBSIDIAN_CORTEX_LOG env var > this value > info
log-level: info

actions:
  naming:
    style: lowercase-hyphenated
    max-length: 80
    # Top-level folders are exempt (emoji prefix convention)
    exempt-patterns:
      - "^[\\p{Emoji}].*/$"

  frontmatter:
    required: [title, date, type, tags]
    type-fields:
      video: [source, channel, url]
      meeting: [scope, company, attendees]
      article: [source, author]
      research: [source]
      book: [author]
      link: [url]
    # If title is missing, derive from filename
    auto-title: true

  tags:
    style: lowercase-hyphenated
    canonical:
      - ai-llm
      - sre
      - kubernetes
      - football
      - obsidian
      - rust
      - python
      - nixos
      - writing
      - music
      - spanish
    aliases:
      ai: ai-llm
      ML: ai-llm
      ml: ai-llm
      k8s: kubernetes
      kube: kubernetes
      nix: nixos

  scope:
    rules:
      - match: { tags: [sre, dat, tatari, engprog] }
        set: { scope: work, company: tatari }
      - match: { source-contains: "granola" }
        set: { scope: work, confidential: true }

  linking:
    scan-for: [people, projects, concepts]
    # Known entities to always link
    entities:
      people: []     # populated over time
      projects: []   # populated over time

  intel:
    daily-note: true
    weekly-review: true
    fabric-patterns: [extract_wisdom, summarize]
    # AI-originated output goes here (not core vault)
    output-path: "⚙️ System/ai-output"

  duplicates:
    # Similarity threshold (0.0-1.0) for fuzzy matching
    threshold: 0.85
    # Only compare notes with the same type
    same-type-only: false

  broken-links:
    # Report broken wikilinks
    check-wikilinks: true
    # Report broken external URLs (slower)
    check-urls: false

state:
  cache-dir: ".cortex"  # relative to vault root

daemon:
  # Actions to run on change detection
  on-change:
    - lint
    - broken-links
  # Debounce: wait N seconds after last change before running
  debounce-secs: 5
  # Watch strategy: "notify" (fs events via notify crate) or "poll" (state fingerprint every N seconds)
  watch: notify
  # Only used when watch: poll
  poll-interval: 300

# Vault structure migrations - define source/target folder mappings
migrations:
  # Example: flatten domain folders into Notes/ with scope frontmatter
  # - name: flatten-to-notes
  #   moves:
  #     - from: "🤖 Tech/**"
  #       to: "Notes/"
  #       set-frontmatter: { scope: personal }
  #     - from: "💼 Work/**"
  #       to: "Notes/"
  #       set-frontmatter: { scope: work, company: tatari }

llm:
  provider: claude
  model: claude-sonnet-4-6
  api-key: ANTHROPIC_API_KEY
```

All YAML keys are hyphenated. In Rust config structs, fields use underscores with `#[serde(rename = "hyphenated-name")]`:

```rust
#[derive(Deserialize)]
pub struct NamingConfig {
    pub style: String,
    #[serde(rename = "max-length")]
    pub max_length: u32,
    #[serde(rename = "exempt-patterns")]
    pub exempt_patterns: Vec<String>,
}
```

### Implementation Plan

#### Phase 1: Foundation (deterministic, no LLM)

Project scaffold (`cargo init`, Cargo.toml, directory structure) is already done.

**1a. Config + logging + state manifest**
- Implement config loading (fallback chain, serde_yaml, hyphenated YAML keys)
- Log level resolution: `--log-level` > `OBSIDIAN_CORTEX_LOG` env var > config `log-level` > `info`
- Tracing subscriber setup with file + stderr output
- `#[instrument]` on all public functions with key parameters
- Implement vault manifest (path + size + mtime per file, cached to `.cortex/manifest.yml`)
- `cortex state --refresh` and `cortex state --diff` (mtime-based comparison, no hashing)
- `--help` with tool dependency check and XDG log path

**1b. Vault parsing**
- Walk vault directory, skip ignored paths, respect protected list
- Parse each `.md` file: extract YAML frontmatter, body
- Build `Vec<Note>` representation of entire vault

**1c. Naming enforcement**
- Detect non-lowercase-hyphenated filenames
- Generate correct slug from current filename
- `--dry-run`: report violations
- `--apply`: rename file, update all wikilinks that reference old name. Note: Obsidian wikilinks are case-insensitive and omit `.md` extension, so `[[My Note]]` and `[[my-note]]` both resolve. Cortex must handle all variations when updating references. **Batch optimization:** When multiple files are renamed in one run, collect all renames first, then do a single pass through all vault files updating all references at once (not one vault scan per rename).

**1d. Frontmatter validation**
- Check required fields per note type
- Validate date format (YYYY-MM-DD)
- Validate tag format (lowercase-hyphenated)
- Auto-derive title from filename if missing
- `--apply`: insert/fix frontmatter fields

**1e. Tag normalization**
- Resolve aliases to canonical tags
- Detect non-canonical tags (not in canonical list, not lowercase-hyphenated)
- Report orphan tags (used by only one note)
- `--apply`: rewrite tag lists in frontmatter

**1f. Broken link detection**
- Scan for `[[wikilinks]]` in note bodies
- Check each target exists as a file in the vault
- Report broken links with context

**1g. Scope classification**
- Apply config-driven scope rules to notes
- Set `scope`, `company`, `confidential` frontmatter fields
- Rule matching: tag-based, source-based

**1h. Migrate**
- Config-driven vault structure migrations
- Move files between folders, set frontmatter during move
- `--dry-run` shows planned moves; `--apply` executes them
- Update wikilinks after moves (same batch optimization as naming)

#### Phase 2: Intelligence (LLM-powered)

**2a. Duplicate detection**
- Content hash comparison for exact dupes
- TF-IDF or embedding-based similarity for fuzzy dupes
- Report with similarity scores, let user decide

**2b. Wikilink inference**
- Scan note bodies for proper nouns, project names, concept terms
- Match against existing note titles and known entities
- `--dry-run`: suggest links
- `--apply`: insert `[[wikilinks]]` at first mention

**2c. Daily/weekly intelligence**
- Scan recent daily notes and inbox items
- Use Fabric patterns (extract_wisdom, summarize) via LLM
- Generate digest: active projects, pending follow-ups, themes
- Write to daily note under `## Digest` heading or `⚙️ System/ai-output/`

**2d. Daemon mode**
- Filesystem watcher (notify crate) on vault root
- Debounce changes, run configured `on-change` actions on affected files
- systemd user service install/uninstall

## Alternatives Considered

### Alternative 1: Extend obsidian-borg with governance features

- **Description:** Add lint/fix/intel subcommands directly to obsidian-borg
- **Pros:** Single binary, shared config, shared vault parsing code
- **Cons:** Borg is event-driven (URL in, note out); governance is scan-driven. Different execution patterns, different concerns. Borg already has 30+ modules. Adding governance bloats it beyond its purpose.
- **Why not chosen:** Separation of concerns. Borg assimilates, cortex processes. Each tool does one thing well. Shared code can be extracted to a common crate later if warranted.

### Alternative 2: Python script with YAML rules

- **Description:** Quick Python implementation using PyYAML, pathlib, regex
- **Pros:** Faster to prototype, easier to iterate
- **Cons:** Inconsistent with the Rust toolchain (borg is Rust). Slower execution on large vaults. No type safety for config parsing.
- **Why not chosen:** Consistency with obsidian-borg. Rust gives us fast vault scanning, strong typing for config, and a single static binary.

### Alternative 3: Obsidian plugin (TypeScript)

- **Description:** Build governance as an Obsidian community plugin
- **Pros:** Runs inside Obsidian, direct access to vault API, live UI feedback
- **Cons:** Tied to Obsidian runtime. Can't run headless/daemon. Different language (TypeScript). Plugin API is limited for batch operations.
- **Why not chosen:** Cortex needs to run headless (daemon, CI, cron). It's a file-level tool, not a UI tool. However, Obsidian Templater or custom JS can shell out to cortex for structured output - this is the intended integration path.

## Technical Considerations

### Dependencies

Core (Phase 1):
- `clap` 4.x - CLI parsing (derive)
- `serde` + `serde_yaml` - Config and frontmatter parsing
- `eyre` - Error handling
- `tokio` - Async runtime
- `tracing` + `tracing-subscriber` - Structured logging and function instrumentation
- `chrono` - Date handling
- `regex` - Pattern matching (filenames, tags, wikilinks)
- `walkdir` - Directory traversal
- `shellexpand` - Tilde expansion in config paths
- `colored` - Terminal output
- `dirs` - Config directory resolution
- `fs2` - File locking (concurrent access safety)
- `serde_json` - JSON output format

Phase 2 additions:
- `notify` - Filesystem watching (daemon mode)
- `reqwest` - LLM API calls
- `sha2` - Content hashing (duplicate detection only)

### Performance

- CLI mode: mtime-based manifest diff identifies changed files without reading content
- Daemon mode: `notify` crate delivers precise file events, no scanning needed
- Content hashes computed only on demand for duplicate detection
- Typical vault size: hundreds to low thousands of `.md` files - full scan should complete in under 1 second
- Daemon uses filesystem events (not polling) with debounce to avoid thrashing

### Security

- LLM API key resolved via file or environment variable (same `resolve_secret` pattern as obsidian-borg)
- No network calls in Phase 1 (purely local file operations)
- `--apply` operations are file renames and in-place edits - no data leaves the machine
- Config file may contain API keys - ensure `~/.config/obsidian-cortex/` has appropriate permissions

### Testing Strategy

- Unit tests for each module: naming slug generation, frontmatter parsing, tag normalization, wikilink extraction
- Integration tests with a fixture vault (small directory of test `.md` files)
- Snapshot tests for lint output (human and JSON formats)
- `--dry-run` is default for all mutating commands - safe to run without risk

### Rollout Plan

1. Implement config loading + tracing setup + `cortex state` - validate manifest-based change detection works
2. Implement `cortex lint --dry-run` with naming + frontmatter + tags - run against real vault, review output
3. Add `--apply` mode once lint output is trusted
4. Add broken link detection and scope classification
5. Add `cortex migrate` for vault structure migrations
6. Add daemon mode with filesystem watcher
7. Phase 2: LLM-powered features (linking, intel, duplicates)

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Frontmatter parser chokes on edge cases (nested YAML, multi-line strings) | Medium | Medium | Use serde_yaml for parsing; test against real vault notes including borg-generated ones |
| File rename breaks wikilinks elsewhere in vault | Medium | High | Scan entire vault for references before renaming; `--dry-run` default; show affected links in report; batch all renames into one vault pass |
| Daemon thrashes on rapid file changes (e.g., Obsidian auto-save) | Medium | Low | Debounce with configurable delay (default 5s) |
| LLM hallucinations create bad wikilinks | Medium | Medium | `--dry-run` default for link inference; require explicit `--apply`; show confidence scores |
| Config drift between borg and cortex (e.g., vault path) | Low | Medium | Consider shared config crate in future; for now, document that both configs must agree on vault root |
| Large vault (10k+ notes) makes full scan slow | Low | Low | Incremental scanning via state diff; only process changed files |
| Concurrent writes (Obsidian, borg, cortex all active) | Medium | Medium | Cortex uses file-level locking (fs2) for `--apply` writes; daemon debounce avoids racing with Obsidian auto-save; document that borg and cortex should not both `--apply` simultaneously |
| Frontmatter round-trip changes formatting (key order, quoting) | High | Medium | For simple fixes (add field, replace tag), use targeted string replacement on the raw YAML block instead of full parse-serialize. Only re-serialize when structural changes are needed (e.g., adding frontmatter to a file that has none) |
| Emoji in vault paths breaks glob/regex patterns | Low | Medium | Test all path operations against emoji-prefixed folders; use `Path` methods over string operations where possible |

## Open Questions

- [ ] Should cortex share a common crate with obsidian-borg for vault parsing, frontmatter types, and config utilities? Or keep them fully independent for now?
- [ ] For duplicate detection: use TF-IDF (pure Rust, no LLM) or embeddings (requires LLM API)? TF-IDF is simpler but less accurate for semantic similarity.
- [ ] Should the canonical tag list live in the config or in a separate file that both cortex and CLAUDE.md reference?
- [ ] Daemon: use `notify` crate (filesystem events) or poll-based with state fingerprinting? Notify is more responsive but adds complexity.
- [ ] Should `cortex migrate` share any logic with `obsidian-borg migrate`, or are they independent concerns?
- [ ] Migration plan format: inline in config YAML, or separate migration plan files?

## Addendum: Abandoned Approaches

### Vault state hashing (abandoned 2026-03-16)

The original design included a `VaultState` struct with a `vault_hash` field - a SHA-256 computed from sorted (path, size, mtime) tuples across all vault files. The idea was to provide a single content-addressable fingerprint for fast "has anything changed?" checks.

We abandoned this because it solves the same problem as the file watcher but worse. In daemon mode, `notify` already tells you exactly what changed. In CLI mode, a simple mtime manifest diff gives you the same answer (which files changed?) without the overhead of hashing. The cryptographic hash added complexity with no benefit - you never need to know "did the vault change?" as a yes/no. You always need to know *which files* changed, and both the watcher and the manifest diff give you that directly.

The current design uses a plain manifest (path, size, mtime per file) for CLI mode and filesystem events for daemon mode. Content hashing (SHA-256) is reserved exclusively for duplicate detection in Phase 2.

## References

- Vault Governance Brainstorm - `⚙️ System/vault-governance-brainstorm.md` in the obsidian vault
- obsidian-borg - companion ingestion tool, reference architecture
- Obsidian Vault CLAUDE.md - vault conventions that cortex enforces
- Jeffrey Emanuel Rule of Five - design doc methodology
