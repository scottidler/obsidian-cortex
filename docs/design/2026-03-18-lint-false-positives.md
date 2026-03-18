# Design Document: Lint False Positive Reduction

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Running `obsidian-cortex lint` against the production vault produces 902 errors and 192 warnings, but roughly 70% of the errors are false positives. The two biggest sources are: wikilink resolution that doesn't match Obsidian's behavior and missing exclusions for repo meta-files. This design addresses each source with minimal changes to the existing architecture.

## Problem Statement

### Background

The lint command was built with correct-by-default assumptions: filenames match slugs, wikilinks reference slugs, and every `.md` file in the vault is a note. These assumptions held during development against the test vault but break against the production vault, where:

- `obsidian-borg` writes title-case wikilinks in the borg-ledger (e.g., `[[Zone Blocking Families]]` for `zone-blocking-families.md`)
- Repo meta-files (`CLAUDE.md`, `README.md`) exist at the vault root
- Templates contain example wikilink syntax that isn't meant to resolve
- Notes embed images via `![[pasted-image-*.png]]` - these are genuinely broken (the image files are missing from the vault), so the linter is correct to flag them (55 occurrences across 6 files)

### Problem

The high false positive rate makes the lint output untrustworthy. Users cannot distinguish real issues from noise, defeating the purpose of the tool.

**Production lint run (2026-03-18):**

| Rule | Count | Estimated FP Rate |
|------|-------|-------------------|
| broken-links.wikilink | 671 | ~80% (55 are genuinely broken image embeds) |
| frontmatter.required.tags | 211 | ~0% (real, but debatable) |
| tags.format | 66 | ~0% |
| frontmatter.tag-format | 66 | ~0% |
| naming.max-length | 54 | ~0% |
| frontmatter.date-format | 6 | ~0% |
| naming.lowercase-hyphenated | 4 | ~50% (CLAUDE.md, README.md) |
| frontmatter.required.* | 12 | ~33% (meta-files, templates) |
| frontmatter.missing | 4 | ~75% (meta-files, templates) |

### Goals

- Reduce broken-links false positive rate from ~85% to <5%
- Eliminate false positives for repo meta-files and templates
- Add title-based wikilink resolution matching Obsidian's actual behavior
- Keep all changes backward-compatible with existing config files

### Non-Goals

- URL checking (check-urls is already disabled by default)
- Fixing the 211 notes with empty tags (that's an obsidian-borg issue)
- Duplicate detection improvements
- LLM-powered features (Phase 2)

## Proposed Solution

### Overview

Two changes, each independently valuable:

1. **Title-aware wikilink resolution** - Build a title index alongside the stem index
2. **Meta-file exclusions** - Add a global `exclude` list that skips files from all rules

### Architecture

No new modules. Changes touch `links.rs`, `config.rs`, `vault.rs`, and `naming.rs`/`frontmatter.rs` (to respect global excludes).

```
config.rs  -->  VaultConfig gains `exclude: Vec<String>`
                (glob patterns for files to skip from lint rules)

lib.rs     -->  run_lint() produces two lists:
                  all_notes     (full scan, used for link indexes)
                  lintable_notes (all_notes minus excluded, used for violations)

links.rs   -->  lint_broken_links(lintable_notes, all_notes, config)
                builds title + slug indexes from all_notes
                reports violations only for lintable_notes
```

### Change 1: Title-Aware Wikilink Resolution

**Current behavior (links.rs:24-36):**
```rust
let note_stems: HashSet<String> = notes.iter()
    .filter_map(|n| n.path.file_stem()...to_lowercase())
    .collect();
```

A wikilink `[[Zone Blocking Families]]` lowercases to `zone blocking families`, which doesn't match the stem `zone-blocking-families`.

**New behavior:** Build two additional indexes:

1. **Title index:** Map each note's `frontmatter.title` (lowercased) to its path
2. **Slug-of-title index:** Map `to_slug(title)` to its path, catching cases where the wikilink text is the title and the filename is its slug

Resolution order for a wikilink `[[target]]`:
1. Exact stem match (existing) - `target.to_lowercase() in note_stems`
2. Exact title match - `target.to_lowercase() in title_index`
3. Slug match - `to_slug(target) in note_stems`

This covers the borg-ledger pattern where `[[Zone Blocking Families]]` should resolve to `zone-blocking-families.md` (via slug match), and also handles any future case where wikilinks use the exact title text.

**Signature change:** `lint_broken_links` gains an `all_notes` parameter for building indexes, while `lintable_notes` controls which files are checked for violations:

```rust
pub fn lint_broken_links(
    lintable_notes: &[Note],
    all_notes: &[Note],
    config: &BrokenLinksConfig,
) -> Report
```

**Index building (from all_notes, including excluded files):**
```rust
// Existing: stem index
let note_stems: HashSet<String> = all_notes.iter()
    .filter_map(|n| n.path.file_stem()?.to_str().map(|s| s.to_lowercase()))
    .collect();

// New: title index (lowercased titles for exact match)
let title_set: HashSet<String> = all_notes.iter()
    .filter_map(|n| n.frontmatter.title.as_ref())
    .map(|t| t.to_lowercase())
    .collect();
```

**Resolution check (for each wikilink in lintable_notes):**
```rust
let target_lower = link.to_lowercase();
let target_slug = to_slug(&link);  // "Zone Blocking Families" -> "zone-blocking-families"

let exists = note_stems.contains(&target_lower)      // [[note-slug]] exact
    || note_paths.contains(&target_lower)              // [[folder/note]] path
    || title_set.contains(&target_lower)               // [[Full Title]] title match
    || note_stems.contains(&target_slug);              // [[Full Title]] slug-of-title match
```

The key insight: `note_stems` already contains all file stems. Step 3 (slug match) converts the wikilink text into a slug via `to_slug()` and checks it against stems. This handles the common borg-ledger case where `[[Zone Blocking Families]]` slugifies to `zone-blocking-families`, matching the file `zone-blocking-families.md`.

### Change 2: Global File Exclusions

**Current state:** `vault.ignore` skips directories. `naming.exempt-patterns` skips naming rules. `frontmatter.path-exempt` skips specific frontmatter fields. There is no way to exclude a file from ALL rules.

**New config field:**

```yaml
vault:
  exclude:
    - "CLAUDE.md"
    - "README.md"
    - "system/templates/**"
```

These files are still scanned by `scan_vault()` (so they appear in the title/stem indexes for link resolution) but are skipped by all lint rules.

**Implementation:** Add `exclude: Vec<String>` to `VaultConfig`. In `run_lint()` (lib.rs), build two lists after scanning:

```rust
let all_notes = scan_vault(&vault_root, &config.vault)?;

let lintable_notes: Vec<&Note> = all_notes.iter()
    .filter(|n| !is_excluded(&n.path, &config.vault.exclude))
    .collect();
```

The `is_excluded` function uses glob matching (same as `path-exempt`). `all_notes` is passed to `lint_broken_links` for index building; `lintable_notes` is passed to all rules for violation checking.

**Why not just add more path-exempt entries?** Because `path-exempt` is per-field within frontmatter only. CLAUDE.md needs to be excluded from naming, frontmatter, tags, and broken-links. A global exclude avoids duplicating the same pattern across every rule config.

### Implementation Plan

**Phase 1: Global excludes**
- Files: `config.rs`, `lib.rs`
- Add `exclude: Vec<String>` to `VaultConfig` with serde default
- Add `is_excluded(path, patterns) -> bool` helper using `glob::Pattern`
- Split `run_lint()` into `all_notes` / `lintable_notes` lists
- Update `lint_broken_links` signature to accept both lists
- Pass `lintable_notes` to all other rules (naming, frontmatter, tags, scope)
- Default excludes: none (conservative; users add their own)
- Tests: excluded files produce zero violations but still appear in link indexes

**Phase 2: Title-aware resolution**
- Files: `links.rs`
- Build `title_set` from `all_notes` frontmatter titles
- Add `to_slug(target)` check against `note_stems`
- Resolution order: stem -> path -> title -> slug-of-target
- Tests: `[[Title Case Link]]` resolves to `title-case-link.md`; `[[nonexistent]]` still flagged

**Phase 3: Config and defaults**
- Files: `obsidian-cortex.yml`, `config.rs` (Default impl)
- Add recommended excludes to shipped config: `CLAUDE.md`, `README.md`, `system/templates/**`
- Keep `VaultConfig::default()` conservative (empty exclude list)

## Alternatives Considered

### Alternative 1: Fuzzy Slug Matching Only

- **Description:** Instead of building a title index, normalize both the wikilink target and all filenames through `to_slug()` and compare.
- **Pros:** Simpler, no need to read frontmatter titles.
- **Cons:** `to_slug("You're not stupid")` produces `youre-not-stupid` but the actual file is `you-re-not-stupid.md`. The slug algorithm strips apostrophes rather than replacing them with hyphens, so there's a normalization mismatch. This catches ~95% of cases but not all.
- **Why not chosen:** Title matching catches 100% of cases where borg writes the title into the ledger. Slug matching alone still has edge cases around punctuation normalization.

### Alternative 2: Suppress Violations for Protected Files

- **Description:** Since `borg-ledger.md` is already in `vault.protected`, skip it during lint.
- **Pros:** Zero code changes to resolution logic.
- **Cons:** Only fixes borg-ledger (557/671 broken links). Doesn't fix title-case wikilinks in other notes. Treats the symptom, not the cause.
- **Why not chosen:** Title-aware resolution is the correct fix and also benefits non-protected files.

### Alternative 3: Per-Rule Exclude Lists

- **Description:** Add `exclude` to each rule config (naming, frontmatter, tags, broken-links).
- **Pros:** Maximum flexibility per rule.
- **Cons:** Config bloat. CLAUDE.md needs to be excluded from 4 different rule sections. Most excludes apply globally.
- **Why not chosen:** Global `vault.exclude` covers the common case. Per-rule exemptions already exist where needed (`naming.exempt-patterns`, `frontmatter.path-exempt`).

## Technical Considerations

### Dependencies

No new crate dependencies. Uses existing `glob` crate (already a dependency for `path-exempt`).

### Performance

- Title index: O(n) to build, O(1) lookup. Negligible for typical vault sizes (<5000 notes).
- Global exclude filter: O(n * p) where p is number of exclude patterns. Tiny.
- No regression to the hot path (note scanning, frontmatter parsing).

### Testing Strategy

- Unit tests for each change (title resolution, exclude filtering)
- Integration test: run full lint against TestVault with title-case wikilinks and meta-files
- Regression test: ensure currently-detected true positives are still caught
- Manual validation: re-run against production vault and compare counts

### Rollout Plan

1. Implement and test in obsidian-cortex repo
2. Run against production vault, verify error count drops from ~900 to ~100-200
3. Bump version, install

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Title resolution hides genuinely broken links | Low | Medium | Resolution is additive - only adds more ways to match, never removes. A link that resolves to a real note is not broken. |
| Global exclude accidentally skips files that should be linted | Low | Low | Excludes require explicit config. Defaults are conservative (only CLAUDE.md, README.md). |
| Slug normalization diverges from obsidian-borg's slugification | Medium | Low | Both use the same algorithm conceptually; edge cases in punctuation handling are caught by title matching as a fallback. |

## Open Questions

- [ ] Should `vault.protected` files also be excluded from lint by default, or should scanning protected files remain opt-in? The borg-ledger contains hundreds of wikilinks that are legitimate testing targets once title resolution works.
- [ ] Should empty `tags: ` (key present, value null/empty) be treated as "present but empty" (warning) rather than "missing" (error)? This affects 211 notes but is orthogonal to the false positive work.
- [ ] Should `extract_wikilinks()` skip wikilinks inside fenced code blocks? CLAUDE.md has `[[wikilinks]]` as example text in code blocks. The global exclude handles CLAUDE.md specifically, but other notes could have the same pattern. This is a separate concern but worth noting.

## References

- [obsidian-cortex design doc](2026-03-16-obsidian-cortex.md) - original architecture
- [schema-alignment design doc](2026-03-18-schema-alignment.md) - enum validation, field migration
- Obsidian wikilink resolution docs: Obsidian resolves `[[target]]` by searching all note titles, then filenames, case-insensitively
- Production lint run: 2026-03-18, 902 errors / 192 warnings / 3244 info
