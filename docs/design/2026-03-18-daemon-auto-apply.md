# Design Document: Daemon Auto-Apply

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The daemon currently watches for vault changes and runs actions in read-only mode. This design adds a config-driven `auto-apply` map so individual actions can be promoted from report-only to auto-fix, giving the user a progressive trust ramp from passive observer to active enforcer.

## Problem Statement

### Background

The daemon (`cortex daemon --start`) watches the vault for file changes and runs configured actions on each debounce cycle. Today, every action runs in report-only mode with `dry_run: true` and `apply: false` hardcoded in `run_configured_actions()` (daemon.rs:121-197). The only writes are the state manifest (`.cortex/manifest.yml`) and intel daily digests (`system/ai-output/`).

The user wants the daemon to eventually enforce vault standards automatically - rename files, normalize tags, fix frontmatter, insert wikilinks - without manual `--apply` runs. But promoting all actions at once is risky: a bad naming rule could bulk-rename hundreds of files, a scope rule could overwrite frontmatter across the vault.

### Problem

There is no way to selectively enable auto-apply for individual daemon actions. The only path from "read-only daemon" to "enforcing daemon" is editing Rust source code.

### Goals

- Per-action `auto-apply` toggle in config, defaulting to `false` for every action
- When enabled, the daemon runs that action with `apply: true` instead of `dry_run: true`
- Actions that write new files (intel, state) are unaffected - they already write unconditionally
- Log every auto-applied change at `info` level with the file path and what changed
- Prevent feedback loops: daemon-triggered writes must not trigger another daemon cycle

### Non-Goals

- Undo/rollback mechanism (git handles this - the vault is a git repo)
- Per-file or per-directory apply scoping (can be added later)
- Rate limiting or batch size caps (the debounce window already throttles)
- Interactive approval (the daemon runs unattended)
- Changing what actions the daemon runs (the existing `on-change` list handles that)

## Proposed Solution

### Overview

Add an `auto-apply` map to the `daemon` config section. Each key is an action name matching the `on-change` list. Each value is a boolean. When the daemon runs an action and its `auto-apply` entry is `true`, it passes `apply: true` instead of `dry_run: true`.

### Config Design

```yaml
daemon:
  debounce-secs: 5
  actions:
    lint:
      apply: false       # naming, frontmatter, tags, scope
    broken-links: {}     # read-only, no apply mode
    link:
      apply: false       # insert wikilinks
    state: {}            # always writes manifest
```

To enable auto-apply for lint and link:
```yaml
daemon:
  actions:
    lint:
      apply: true
    broken-links: {}
    link:
      apply: true
    state: {}
```

**Semantics:**
- An action is enabled by being present in the `actions` map.
- `apply` defaults to `false` (report-only) when omitted or `{}`.
- Actions that have no apply mode (broken-links, state, intel) ignore the `apply` setting.

### Data Model

```rust
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    #[serde(rename = "on-change")]
    pub on_change: Vec<String>,
    #[serde(rename = "debounce-secs")]
    pub debounce_secs: u64,
    pub watch: String,
    #[serde(rename = "poll-interval")]
    pub poll_interval: u64,
    #[serde(rename = "auto-apply")]
    pub auto_apply: HashMap<String, bool>,
}
```

Default for `auto_apply`: empty HashMap (everything off).

### Implementation Plan

**Phase 1: Config + plumbing**
- Add `auto_apply: HashMap<String, bool>` to `DaemonConfig`
- Add helper: `fn should_apply(&self, action: &str) -> bool`
- Update `run_configured_actions()` to check `should_apply()` per action
- When `should_apply("lint")` is true, pass `apply: true, dry_run: false`
- When `should_apply("link")` is true, pass `apply: true, dry_run: false`

**Phase 2: Feedback loop prevention**
- Add an `applying: AtomicBool` flag to the daemon's watch loop
- Set it to `true` before running auto-apply actions, `false` after
- When `applying` is true, discard all incoming file change events
- This is simple and correct: the daemon processes changes sequentially, so any events during an apply cycle are self-triggered
- Alternative: track which files were just written and filter those out. More precise but more complex and unnecessary for v1.

**Phase 3: Logging**
- Each action already returns a report with violation counts
- When auto-applying, log at info: `[daemon] auto-applied lint: 3 file(s) modified`
- When reporting only, log as today: `[daemon] lint: 5 violation(s)`

**Phase 4: Sub-action granularity (future)**
- Allow `auto-apply.lint` to be a map instead of a bool: `{ naming: true, tags: true, frontmatter: false, scope: false }`
- Requires refactoring `run_lint` to accept per-rule apply flags
- Not needed for v1 - action-level control is sufficient to start

### Code Changes

`src/config.rs` - Add `auto_apply` field to `DaemonConfig`:
```rust
#[serde(rename = "auto-apply", default)]
pub auto_apply: HashMap<String, bool>,
```

`src/daemon.rs` - Update `run_configured_actions()`:
```rust
"lint" => {
    let should_apply = daemon_config.auto_apply.get("lint").copied().unwrap_or(false);
    let opts = crate::cli::LintOpts {
        dry_run: !should_apply,
        apply: should_apply,
        format: "human".to_string(),
        rule: Vec::new(),
        path: None,
    };
    match crate::run_lint(vault_root, config, &opts) {
        Ok(report) => {
            let count = report.violations.len();
            if should_apply {
                tracing::info!(violations = count, "auto-applied lint");
                println!("[daemon] auto-applied lint: {count} fix(es)");
            } else if !report.is_empty() {
                println!("[daemon] lint: {count} violation(s)");
            }
        }
        Err(e) => tracing::error!(error = %e, "lint action failed"),
    }
}
```

Same pattern for `"link"`.

## Alternatives Considered

### Alternative 1: Global auto-apply flag
- **Description:** Single `auto-apply: true/false` on the daemon config
- **Pros:** Simpler config
- **Cons:** All-or-nothing. Can't enable tag normalization without also enabling file renames.
- **Why not chosen:** Defeats the purpose of progressive trust. The user should be able to enable the safe stuff first.

### Alternative 2: Separate daemon profiles (e.g. `daemon --mode=enforce`)
- **Description:** CLI flag that switches the daemon between report and enforce modes
- **Pros:** Clear modal distinction
- **Cons:** Requires restarting the daemon to change mode. Still all-or-nothing unless combined with per-action config.
- **Why not chosen:** Per-action config is more flexible and doesn't require daemon restarts.

### Alternative 3: Sub-action granularity (e.g. `auto-apply: { naming: true, tags: true, frontmatter: false }`)
- **Description:** Break `lint` into its sub-actions for auto-apply control
- **Pros:** Maximum control - enable tag normalization without enabling file renames
- **Cons:** Requires refactoring `run_lint` to accept per-rule apply flags. More config complexity.
- **Why not chosen:** Good future enhancement, but overkill for v1. Start with action-level granularity. The `lint` action is already the coarsest unit that takes `--apply`. Sub-action control can be added later by splitting `auto-apply.lint` into a map.

## Technical Considerations

### Dependencies

None new. Uses existing `HashMap` from std and existing serde deserialization.

### Performance

No change. The daemon already runs these actions on every debounce cycle. Apply mode does more I/O (writes files) but the vault is small enough that this is negligible.

### Security

The daemon runs as the user. Auto-apply modifies files in the vault. The vault is a git repo, so all changes are visible in `git diff` and recoverable via `git checkout`. No elevated privileges involved.

### Testing Strategy

- Unit test: `DaemonConfig` deserialization with and without `auto-apply`
- Unit test: `should_apply()` returns correct values for present/absent/false entries
- Integration test: run daemon action cycle with `auto-apply.lint: true` on a temp vault, verify files were modified
- Manual test: enable `auto-apply.lint: true` in real config, start daemon, edit a note, verify auto-fix

### Rollout Plan

1. Merge with all `auto-apply` defaults to `false` - zero behavior change
2. User enables one action at a time in their config as confidence grows
3. Document recommended progression: `lint` (tags, frontmatter) first, then `link`, naming last

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Bad rule bulk-modifies files | Medium | High | All defaults off. Vault is git-tracked. User enables one at a time. |
| Daemon applies during active editing | Low | Medium | Debounce window (5s default) prevents mid-edit triggers. Obsidian handles external file changes gracefully. |
| Config typo enables unintended action | Low | Low | Unknown action names are ignored (no matching key in auto-apply map). Only exact matches activate. |
| Feedback loop (apply triggers re-apply) | High | Low | `applying` flag suppresses events during auto-apply cycle. Even without the flag, apply functions are idempotent (only write when content actually changes), so a re-trigger would be a no-op. The flag prevents wasted CPU, not data corruption. |
| Intel writes conflict with auto-apply writes | Low | Low | Intel writes to `system/ai-output/` (new files). Lint/link modify note bodies. No overlap. |

## Open Questions

- [ ] Should `auto-apply` support sub-action granularity for `lint` in v1, or defer to v2?
- [ ] Should the daemon log a summary of all auto-applied changes to a vault file (e.g. `.cortex/changelog.md`) for auditability beyond git?

## References

- Daemon implementation: `src/daemon.rs`
- Config schema: `src/config.rs` (DaemonConfig, lines 273-294)
- Existing design docs: `docs/design/2026-03-18-wikilink-severity.md`
