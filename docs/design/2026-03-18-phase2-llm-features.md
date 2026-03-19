# Design Document: Phase 2 - LLM-Powered Features

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Phase 2 extends obsidian-cortex from deterministic linting/linking into LLM-powered intelligence. This covers four discrete phases: surfacing duplicate detection results via Dataview-queryable tags, wiring Fabric patterns into the intel module, adding LLM-powered auto-tagging for new notes, and content quality scoring for stale/empty/orphaned notes.

## Problem Statement

### Background

Phase 1 built the deterministic foundation: frontmatter validation, naming, tags, broken links, wikilink inference, scope classification, and a daemon that auto-applies fixes. The duplicate detection engine (TF-IDF cosine similarity) and intel module (daily/weekly digests) exist but are incomplete - duplicates only report to logs, intel doesn't use Fabric, and there's no LLM-powered content analysis.

### Problem

1. **Duplicate detection is invisible** - `lint_duplicates` finds exact and fuzzy matches but results only appear in CLI output/logs. Users have no way to discover or act on duplicates from within Obsidian.
2. **Intel reports are static** - daily/weekly digests list notes and tags but lack synthesis. The Fabric integration exists (`fabric.rs`) but isn't called.
3. **No content intelligence** - new notes arrive via obsidian-borg without tags beyond what ingestion provides. Stale, empty, and orphaned notes accumulate silently.

### Goals

- Surface duplicate pairs in Obsidian via Dataview-queryable frontmatter
- Generate LLM-enhanced intel reports using Fabric patterns
- Auto-suggest tags for notes based on content analysis
- Identify and flag low-quality notes (stale, empty, orphaned)

### Non-Goals

- Auto-merging duplicates (too destructive for automation)
- Real-time LLM processing on every file save (cost/latency)
- Replacing obsidian-borg's ingestion pipeline
- Building a custom UI - Dataview is the rendering layer

## Design Principle: Frontmatter as Communication Channel

All Phase 2 features follow a single pattern: cortex writes machine-readable frontmatter fields (prefixed `cortex-`), and the user queries them via Dataview. Cortex never builds UI. This keeps the tool headless, the data portable, and the user in control of presentation.

The `cortex-` prefix creates a clear namespace boundary: user-curated fields (`title`, `tags`, `domain`) vs cortex-managed fields (`cortex-duplicate`, `cortex-quality`, `cortex-suggested-tags`). Cortex only writes to its own namespace and never modifies user fields without explicit action (e.g., `apply` commands).

## Proposed Solution

### Phase 2a: Duplicate Surfacing

**Problem:** Duplicates are detected but invisible inside Obsidian.

**Approach:** When duplicates are found, write machine-readable frontmatter that Dataview can query.

Two frontmatter fields per duplicate note:

```yaml
cortex-duplicate: true
cortex-duplicate-group: "dup-a1b2c3"
```

- `cortex-duplicate: true` - a simple flag for Dataview filtering (`WHERE cortex-duplicate = true`)
- `cortex-duplicate-group` - a short hash derived from the duplicate pair/cluster, so Dataview can group related duplicates together (`GROUP BY cortex-duplicate-group`)

For exact duplicates, the group hash is the content hash (already computed via FNV-1a). For fuzzy matches, use the stem of the oldest note in the cluster as the group anchor (e.g., `dup-my-oldest-note`). This is stable across runs and human-readable in Dataview.

**Clearing:** When a note is modified and no longer matches its duplicate group, the daemon removes both fields on the next sweep. This file write will trigger the daemon's watcher, but the existing cycle detection (sweep fingerprinting) prevents oscillation - the second sweep will see no changes and back off.

**Dataview query example:**

```dataview
TABLE title, cortex-duplicate-group as "Group", file.mtime as "Modified"
FROM ""
WHERE cortex-duplicate = true
GROUP BY cortex-duplicate-group
SORT file.mtime DESC
```

**Implementation:**

1. Add `Fix::SetDuplicateFields { group_hash }` variant to `report.rs`
2. Extend `apply_fixes` to write `cortex-duplicate` and `cortex-duplicate-group` into frontmatter using `scope::insert_frontmatter_fields`
3. Add clearing logic: if a note has `cortex-duplicate: true` but no longer appears in duplicate results, remove both fields
4. Wire into daemon actions map as `duplicates` with `apply: true/false`

### Phase 2b: Fabric-Powered Intel

**Problem:** Daily/weekly digests are mechanical note lists without synthesis.

**Approach:** Pipe note content through Fabric patterns to generate summaries, extract insights, and identify themes.

**Integration points:**

1. **New note processing** - when the daemon sees a new note (or one with `status: unread`), run `fabric --pattern extract_wisdom` on its body and write results to a `cortex-insights` frontmatter field (not the body - avoids invasive edits to user content)
2. **Daily digest enhancement** - after building the note list, concatenate today's note bodies and run `fabric --pattern summarize` to produce a narrative summary
3. **Weekly review enhancement** - run `fabric --pattern extract_wisdom` on the concatenated week's notes to surface cross-cutting themes

**Config additions:**

```yaml
actions:
  intel:
    fabric-patterns:
      - extract_wisdom
      - summarize
    on-new-note: extract_wisdom    # pattern to run on new/unread notes
    batch-daily: summarize         # pattern for daily digest synthesis
    batch-weekly: extract_wisdom   # pattern for weekly review synthesis
    max-input-tokens: 50000        # truncate input to avoid cost blowup
```

**Fallback:** If `fabric` binary is not available (`fabric::is_available()` returns false), skip LLM features gracefully and generate the existing static reports.

**Implementation:**

1. Add `on_new_note` processing to intel module - detect notes with `status: unread`, run pattern, write `cortex-insights` to frontmatter, set `status: processed`
2. Enhance `generate_daily_digest` to call `fabric::run_pattern("summarize", &concatenated_bodies)`
3. Enhance `generate_weekly_review` to call `fabric::run_pattern("extract_wisdom", &concatenated_bodies)`
4. Add `max-input-tokens` config to prevent cost blowup on large vaults
5. Wire daily/weekly generation into daemon as a poll-interval action. The daemon doesn't have time-of-day awareness, so intel generation triggers on the periodic full sweep (configurable via `poll-interval`). For true daily scheduling, rely on systemd timers or cron calling `obsidian-cortex intel --daily`

### Phase 2c: LLM Auto-Tagging

**Problem:** Notes arrive from obsidian-borg with basic tags but miss semantic connections.

**Approach:** Use Fabric to suggest tags for notes that have few or generic tags.

**Trigger criteria:**
- Notes with fewer than N tags (configurable, default 3)
- Notes with `status: unread` or `origin: assisted` (freshly ingested)
- Exclude notes already processed (track via `cortex-tagged: true` field)

**Process:**

1. Build a prompt including the note body and the vault's canonical tag list (from `config.tags.canonical`). **Prerequisite:** the canonical tag list must be populated - either manually in config or auto-derived from the vault's existing tag corpus (top N tags by frequency)
2. Run through a Fabric pattern (custom `suggest_tags` pattern or `extract_wisdom` post-processed)
3. Parse suggested tags, filter against canonical list, present as frontmatter additions
4. Write suggested tags into a `cortex-suggested-tags` frontmatter field (not directly into `tags`) so the user can review and accept

```yaml
cortex-suggested-tags:
  - rust
  - cli-tools
  - automation
cortex-tagged: true
```

**Acceptance workflow:** User reviews in Dataview, moves tags from `cortex-suggested-tags` to `tags`, or ignores. A future CLI command (`cortex accept-tags <note>` or `cortex accept-tags --all`) could automate bulk acceptance.

**Dataview query:**

```dataview
TABLE title, cortex-suggested-tags as "Suggested Tags", length(tags) as "Current Tags"
FROM ""
WHERE cortex-suggested-tags
SORT file.mtime DESC
```

**Implementation:**

1. Add `AutoTagConfig` to config (min tags threshold, canonical list reference, pattern name)
2. If `config.tags.canonical` is empty, auto-derive from vault: collect all tags, rank by frequency, use top N as the canonical set
3. Create `autotag.rs` module with `suggest_tags(note, config) -> Vec<String>`
4. Write suggestions to frontmatter as `cortex-suggested-tags`, set `cortex-tagged: true`
5. Wire into daemon as `auto-tag` action

### Phase 2d: Content Quality Scoring

**Problem:** Vaults accumulate stale, empty, and orphaned notes that degrade signal-to-noise.

**Approach:** Score notes on quality dimensions and surface low-quality notes via frontmatter fields.

**Quality signals (deterministic - no LLM needed):**

| Signal | Detection | Score Impact |
|--------|-----------|-------------|
| Empty body | `body.trim().is_empty()` | Critical |
| Stub body | `body.split_whitespace().count() < 50` | Warning |
| No inbound links | Not referenced by any other note's wikilinks | Warning |
| No outbound links | Contains no wikilinks | Info |
| Stale | `date` field older than N days, never modified | Info |
| Missing summary | No `## Summary` or `> [!tldr]` section | Info |

**Frontmatter output:**

```yaml
cortex-quality: low      # low | medium | high
cortex-quality-issues:
  - empty-body
  - no-inbound-links
```

**Dataview query:**

```dataview
TABLE title, cortex-quality as "Quality", cortex-quality-issues as "Issues"
FROM ""
WHERE cortex-quality = "low"
SORT file.mtime ASC
```

**Implementation:**

1. Create `quality.rs` module with `lint_quality(notes, config) -> Report`
2. Build inbound link index (reverse map of wikilink targets)
3. Score each note, write `cortex-quality` and `cortex-quality-issues` to frontmatter
4. Wire into daemon as `quality` action
5. Add clearing logic: re-score on change, update/remove fields

## Alternatives Considered

### Alternative 1: Obsidian Plugin for Duplicate UI

- **Description:** Build an Obsidian plugin that renders duplicate results in a sidebar
- **Pros:** Richer UI, interactive merge workflow
- **Cons:** Requires JS/TypeScript, separate project, breaks headless philosophy
- **Why not chosen:** Cortex's value is being headless and daemon-driven. Dataview already provides queryable UI.

### Alternative 2: Direct Claude API Instead of Fabric

- **Description:** Call Claude API directly from cortex (LlmConfig already exists)
- **Pros:** No external dependency, more control over prompts
- **Cons:** Fabric patterns are battle-tested, community-maintained, and provider-agnostic
- **Why not chosen:** Fabric first, direct API as fallback. Can add later if Fabric patterns are insufficient.

### Alternative 3: Tag Duplicates with Obsidian Tags (#cortex/duplicate)

- **Description:** Use `#cortex/duplicate` in the tags array instead of separate frontmatter fields
- **Pros:** Shows up in Obsidian's native tag pane
- **Cons:** Can't encode the group hash in a tag cleanly, pollutes the tag namespace, harder to clear programmatically
- **Why not chosen:** Frontmatter fields are cleaner for machine-written metadata. Tags should remain user-curated.

## Technical Considerations

### Dependencies

- **Fabric CLI** - required for Phase 2b/2c. Graceful degradation when absent.
- **Existing modules** - `scope::insert_frontmatter_fields` for writing frontmatter, `fabric::run_pattern` for LLM calls
- **Daemon** - all phases integrate as daemon actions with apply: true/false

### Performance

- **Duplicates:** TF-IDF is O(n^2) for pairwise comparison. For vaults under 5,000 notes this is fine. For larger vaults, consider pre-filtering by type/domain or using locality-sensitive hashing.
- **Fabric calls:** Each call is 2-10 seconds. Batch processing (daily/weekly) is acceptable. Per-note processing should be rate-limited and only run on unprocessed notes.
- **Quality scoring:** Deterministic, fast. Inbound link index is O(n) to build.

### Security

- Fabric patterns may send note content to external LLM providers. Config should document this clearly.
- `api-key` field in config should reference env var name, not literal key (already the case).

### Testing Strategy

- **Duplicates surfacing:** TestVault with known duplicate pair, verify frontmatter fields written and cleared
- **Intel + Fabric:** Mock `fabric::run_pattern` in tests (inject a fake binary or trait-based abstraction)
- **Auto-tagging:** Test tag suggestion filtering against canonical list
- **Quality:** TestVault with empty, stub, orphaned, and healthy notes - verify scoring

### Rollout Plan

**Phase dependency graph:**

```
Phase 2a (Duplicates)  ---\
                           >--- all share insert/remove_frontmatter_fields
Phase 2d (Quality)     ---/    and daemon action wiring
                               (can be built in parallel)

Phase 2b (Intel+Fabric) ---\
                            >--- both depend on fabric.rs
Phase 2c (Auto-Tagging) ---/    (build 2b first, 2c reuses the pattern)
```

Recommended order: **2a -> 2d -> 2b -> 2c**. Phases 2a and 2d are deterministic (no LLM dependency), build out the frontmatter-as-communication pattern and the field removal utility, then layer on LLM features.

All phases default to `apply: false` in daemon config, requiring explicit opt-in.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Frontmatter bloat from cortex-* fields | Medium | Low | Prefix all fields with `cortex-`, document cleanup |
| Fabric CLI not installed on all systems | Medium | Low | Graceful fallback, skip LLM features |
| LLM costs from processing many notes | Medium | Medium | max-input-tokens config, process only unread notes |
| TF-IDF false positives on short notes | Medium | Low | Minimum body length threshold, tunable similarity threshold |
| Daemon write conflicts with Obsidian | Low | Medium | Existing fs2 file locking, debounce |
| Cortex-written frontmatter confuses users | Low | Medium | Clear `cortex-` prefix convention, document in vault README |
| Duplicate flag thrashing near threshold | Medium | Low | Hysteresis: only clear `cortex-duplicate` when score drops below `threshold - 0.05` |
| Fabric process hangs, blocks daemon | Low | High | Add configurable timeout (default 30s) to `fabric::run_pattern`, kill on timeout |
| Empty notes all hash to 0, false exact dupes | High | Medium | Skip notes with empty/whitespace-only bodies in duplicate detection |
| Quality flags system-generated notes unfairly | Medium | Low | Exclude notes with `type: digest` or `type: review` from quality scoring |
| No `remove_frontmatter_fields` function | - | - | Must implement field removal in `scope.rs` for clearing logic (new utility needed) |

## Open Questions

- [x] Should `cortex-duplicate-group` use a short hash or the stem of the "primary" note in the group? **Resolved:** FNV-1a hash for exact dupes, oldest stem for fuzzy clusters
- [ ] For auto-tagging, should we create a custom Fabric pattern or post-process `extract_wisdom` output?
- [ ] Should quality scoring be purely deterministic (Phase 2d) or eventually incorporate LLM assessment?
- [ ] What's the right daemon schedule for intel generation - daily at a fixed time, or on demand only?
- [ ] Should `cortex-suggested-tags` auto-merge into `tags` after N days with no user action?

## References

- Phase 1 design: `docs/design/2026-03-16-obsidian-cortex.md`
- Fabric patterns: `~/repos/danielmiessler/fabric/patterns/`
- Jeffrey Emanuel Rule of Five: `~/repos/scottidler/obsidian/notes/jeffrey-emanuel-rule-of-five-agentic-llm.md`
- Daemon design: `docs/design/2026-03-18-daemon-auto-apply.md`
- Cycle detection: `docs/design/2026-03-18-cycle-detection.md`
