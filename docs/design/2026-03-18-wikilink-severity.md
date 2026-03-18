# Design Document: Wikilink Severity Classification

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The broken-links linter treats all unresolved wikilinks as errors, but Obsidian deliberately supports unresolved `[[wikilinks]]` as "future note" placeholders. This design classifies unresolved wikilinks by type so that genuinely broken references (missing images, stale folder links) surface as errors while aspirational note links are reported at info severity.

## Problem Statement

### Background

After the lint false positive reduction work (2026-03-18-lint-false-positives.md), broken-links violations dropped from 671 to 107. Analysis of the remaining 107 shows three distinct categories:

| Category | Count | Example | Actually broken? |
|----------|-------|---------|-----------------|
| Missing assets (images, PDFs) | 60 | `![[pasted-image-20240617071506.png]]` | Yes - file missing from vault |
| Stale folder links | 7 | `[[📥 Inbox/]]` | Yes - v1 folder refs, no longer valid |
| Unresolved note links | ~36 | `[[tmux]]`, `[[ThePrimeagen]]` | No - aspirational future notes |
| Wikilinks in code blocks | several | `[[wikilinks]]` in fenced blocks | No - example syntax, not real links |

### Problem

Reporting `[[tmux]]` and `[[ThePrimeagen]]` as errors is a false positive. In Obsidian, unresolved wikilinks are a feature: they appear purple in the editor, show as ghost nodes in the graph, and clicking one creates the note. Treating them as errors punishes a core PKM workflow.

Meanwhile, `![[pasted-image-20240617071506.png]]` referencing a file that does not exist in the vault is genuinely broken - the image will never render.

### Goals

- Classify missing asset references (images, PDFs, etc.) as errors
- Classify unresolved note links as info (aspirational, not broken)
- Skip wikilinks inside fenced code blocks
- Reduce broken-links errors from 107 to ~67 (assets + stale folders only)
- Keep all categories visible in lint output at their appropriate severity

### Non-Goals

- Auto-creating stub notes for unresolved links
- Checking whether image files exist on disk (beyond stem matching)
- URL validation (already handled by check-urls, disabled by default)
- Fixing the 60 missing pasted-image files (that's a vault content issue)
- Handling wikilinks inside inline code backticks (low occurrence, low risk)

## Proposed Solution

### Overview

Modify `extract_wikilinks()` to skip fenced code blocks before extracting. Modify `lint_broken_links()` to assign severity based on whether the unresolved target looks like an asset or a note.

### Classification Rules

For each unresolved wikilink target:

```
Is it inside a fenced code block?
  -> Skip entirely (not a real link)

Does the target have an asset file extension (.png, .jpg, .pdf, etc.)?
  -> ERROR  rule: broken-links.asset

Does the target end with "/"?
  -> ERROR  rule: broken-links.folder

Otherwise:
  -> INFO   rule: broken-links.unresolved
```

### Asset Extensions

Recognized as asset references when the target ends with one of:

```
Images:    .png, .jpg, .jpeg, .gif, .svg, .webp, .bmp, .tiff
Documents: .pdf
Media:     .mp4, .mp3, .wav, .webm, .ogg, .m4a
Other:     .csv, .excalidraw
```

This list covers Obsidian's supported embed types. Note `.md` is not in this list - a `[[nonexistent.md]]` reference is treated as an unresolved note link, which is correct.

### Architecture

All changes confined to `links.rs`. No new modules, no config changes, no new dependencies.

```
links.rs  ->  strip_fenced_code_blocks(body) -> String
              called before extract_wikilinks() to remove ```...``` blocks

          ->  is_asset_reference(target) -> bool
              checks if target ends with a known asset extension

          ->  lint_broken_links() severity logic:
              is_asset_reference(target)  -> Severity::Error,  "broken-links.asset"
              target.ends_with('/')       -> Severity::Error,  "broken-links.folder"
              otherwise                   -> Severity::Info,   "broken-links.unresolved"
```

The existing `broken-links.wikilink` rule name is retired in favor of the three sub-rules above. This is backward-compatible: any tooling filtering on `broken-links` prefix still matches all three.

### Implementation Plan

**Phase 1: Code-block filtering and severity classification**
- Files: `links.rs`
- Add `strip_fenced_code_blocks(body: &str) -> String` that removes content between ``` fences
- Call it in `lint_broken_links` before passing body to `extract_wikilinks`
- Add `is_asset_reference(target: &str) -> bool` checking file extensions
- Replace single `broken-links.wikilink` violation with three classified rules
- Tests:
  - Wikilinks inside fenced code blocks are not extracted
  - `![[image.png]]` targeting missing file -> `broken-links.asset` at error severity
  - `[[some-concept]]` targeting nothing -> `broken-links.unresolved` at info severity
  - `[[Old Folder/]]` targeting nothing -> `broken-links.folder` at error severity
  - Existing valid-link and broken-link tests still pass

## Alternatives Considered

### Alternative 1: Config-based allowlist for known aspirational links

- **Description:** Add a `broken-links.allow` list in config for known future notes.
- **Pros:** Explicit control over what's suppressed.
- **Cons:** Requires manual maintenance. Every new aspirational link needs a config update. Defeats the purpose of unresolved links as a lightweight workflow.
- **Why not chosen:** The classification is deterministic - assets have file extensions, notes don't. No config needed.

### Alternative 2: Separate embed extraction regex

- **Description:** Use separate regexes for `![[embed]]` and `[[link]]`, treat embeds as errors and links as info.
- **Pros:** Simple - embed syntax (`![[...]]`) implies an asset.
- **Cons:** Not all asset references use embed syntax. A note could contain `[[document.pdf]]` without the `!` prefix. Extension-based detection is more reliable.
- **Why not chosen:** Extension check catches all cases regardless of whether `!` prefix was used.

### Alternative 3: Warn severity instead of info for unresolved notes

- **Description:** Use `Severity::Warning` for unresolved note links instead of info.
- **Pros:** More visible in output.
- **Cons:** Warnings imply something should be fixed. Unresolved note links are intentional in Obsidian.
- **Why not chosen:** Info is the correct severity for informational, non-actionable findings. Users who want to track future notes can filter on `broken-links.unresolved`.

## Technical Considerations

### Dependencies

No new crate dependencies. Uses existing `Regex` and `LazyLock`.

### Performance

Code-block stripping adds one pass over the body text per note. Negligible for vault sizes under 5000 notes. The extension check is O(1) per link.

### Testing Strategy

- Unit tests for `strip_fenced_code_blocks`: strips content, preserves non-code text
- Unit tests for `is_asset_reference`: all listed extensions, edge cases
- Unit tests for severity classification: asset -> error, folder -> error, note -> info
- Regression: existing `test_broken_link_detected_on_vault` updated for new rule name
- Manual validation: re-run against production vault, verify error count ~67

### Rollout Plan

1. Implement and test in obsidian-cortex repo
2. Run against production vault, verify error count drops to ~67
3. Bump version, install

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Asset extension list misses an Obsidian-supported type | Low | Low | List covers all documented Obsidian embed types. Easy to extend later. |
| Code-block stripping breaks on nested or indented fences | Low | Low | Simple line-by-line state tracking. Nested fences are rare in vault notes. |
| Splitting rule names breaks downstream tooling | Low | Low | All three sub-rules share the `broken-links.` prefix. Prefix filtering still works. |

## Open Questions

- [ ] Should `broken-links.asset` and `broken-links.unresolved` be independently toggleable in config, or is severity classification sufficient?
- [ ] `[[note#heading]]` anchor links: the current regex captures "note#heading" as the full target, which won't match any stem. This is a pre-existing issue, not in scope here, but worth fixing separately.
- [ ] Should wikilinks inside inline code (single backticks) also be skipped? Lower priority since it's rarer and harder to detect without a full markdown parser.

## References

- [Lint false positives design doc](2026-03-18-lint-false-positives.md) - predecessor work
- Obsidian wikilink behavior: unresolved links are a deliberate feature, shown as purple text in the editor
- Production lint run: 2026-03-18, 107 broken-links violations post false-positive reduction
