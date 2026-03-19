# Design Document: Daemon Cycle Detection

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

The daemon's periodic sweep can oscillate if an apply function produces output that triggers itself on the next run. This design adds a lightweight cycle detector that compares consecutive sweep results and backs off when it detects repetition.

## Problem Statement

### Background

The daemon runs actions on file changes (reactive) and on a periodic sweep (every `poll-interval` seconds). Actions with `apply: true` modify vault files. The `applying` AtomicBool flag prevents reactive feedback loops (daemon writes don't trigger new watcher events), but the periodic sweep bypasses the watcher entirely - it runs unconditionally on a timer.

### Problem

If an apply function is not perfectly idempotent - for example, a lint fix that reformats frontmatter in a way that creates a new violation - the periodic sweep will apply the same fixes every cycle forever. This wastes CPU, churns file modification times, and could corrupt content if the oscillation involves destructive rewrites.

Current apply functions are idempotent in practice, but there is no safety net if a bug or config change breaks that invariant.

### Goals

- Detect when consecutive sweeps produce identical results
- Back off and log a warning instead of re-applying
- Resume normal operation when a real user edit breaks the cycle
- Zero overhead when sweeps converge to no-ops (the happy path)

### Non-Goals

- Detecting cycles longer than 2 (A -> B -> A). Two-step detection covers the practical case.
- Preventing the first application of a bad fix (that's a correctness issue in the apply functions)
- Persisting cycle state across daemon restarts

## Proposed Solution

### Overview

After each sweep, compute a fingerprint of what was applied. Before the next sweep, compare to the previous fingerprint. If identical and non-empty, skip the apply phase and log a warning. Reset the fingerprint when a real file change event arrives.

### Data Model

```rust
/// Fingerprint of a single sweep's apply results.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SweepFingerprint {
    /// Sorted list of (action, sorted file paths) for actions that applied changes.
    results: Vec<(String, Vec<String>)>,
}

impl SweepFingerprint {
    fn is_empty(&self) -> bool {
        self.results.is_empty() || self.results.iter().all(|(_, files)| files.is_empty())
    }
}
```

The fingerprint includes the exact set of files modified per action, sorted for stable comparison. This avoids false positives where two sweeps fix the same count but different files (convergence, not oscillation).

### Implementation Plan

**Phase 1: Capture sweep results**
- `run_configured_actions` already returns nothing. Change it to return a `SweepFingerprint` built from the violation counts of each apply action.
- For lint: collect file paths from the report's violations that were fixed.
- For link: collect file paths that had wikilinks inserted.
- Read-only actions (broken-links, state) contribute nothing to the fingerprint.
- Sort file paths within each action for stable comparison.

**Phase 2: Compare and back off**
- In `start_watching`, keep a `last_sweep_fingerprint: SweepFingerprint`.
- After each sweep, compare the new fingerprint to `last_sweep_fingerprint`.
- If equal and non-empty: log `warn!("cycle detected, skipping apply")` and do not update the fingerprint (so the next sweep also skips).
- If different or empty: store the new fingerprint as `last_sweep_fingerprint`.

**Phase 3: Reset on user edit**
- When a real file change event arrives (not suppressed by the `applying` flag), reset `last_sweep_fingerprint` to `Default::default()`.
- This re-enables apply on the next sweep, since a user edit may have broken the cycle.

### Code Changes

`src/daemon.rs`:

```rust
// After run_configured_actions returns a fingerprint:
let fingerprint = run_configured_actions(vault_root, config, daemon_config, &[]);

if !fingerprint.is_empty() && fingerprint == last_sweep_fingerprint {
    tracing::warn!(
        fingerprint = ?fingerprint.results,
        "cycle detected: sweep produced same results as previous, skipping apply"
    );
} else {
    last_sweep_fingerprint = fingerprint;
}
```

In the event handling branch, reset when real changes arrive:

```rust
Ok(event) => {
    if should_process_event(&event, &config.vault.ignore) {
        last_sweep_fingerprint = SweepFingerprint::default();
        // ... existing path handling
    }
}
```

## Alternatives Considered

### Alternative 1: Content hashing
- **Description:** Hash the actual file contents before and after each sweep
- **Pros:** Catches any change, not just counts
- **Cons:** Expensive for large vaults. Overkill when we just need to detect repetition.
- **Why not chosen:** Fix counts are sufficient. If counts match, the same fixes were applied. Content hashing adds I/O for no practical benefit.

### Alternative 2: Per-file tracking
- **Description:** Track which specific files were modified and compare the exact set
- **Pros:** More precise than counts alone
- **Cons:** More memory, more complexity. Two sweeps applying 97 fixes to different files would have the same count but different file sets.
- **Why not chosen:** In practice, the same violation count on the same action means the same files. If this proves insufficient, we can upgrade the fingerprint to include file paths later.

### Alternative 3: Max-retry counter
- **Description:** Stop applying after N consecutive non-empty sweeps
- **Pros:** Simple
- **Cons:** Arbitrary threshold. Doesn't distinguish convergence (97 -> 50 -> 12 -> 0) from oscillation (97 -> 97 -> 97).
- **Why not chosen:** Fingerprint comparison is just as simple and correctly handles convergence.

## Technical Considerations

### Performance

Zero overhead on the happy path (fingerprint is empty, comparison is trivial). When there are fixes, the fingerprint is a Vec of 2-4 tuples - negligible.

### Testing Strategy

- Unit test: `SweepFingerprint` equality and `is_empty`
- Unit test: cycle detection logic (same fingerprint twice = skip)
- Unit test: reset on file change clears fingerprint
- Integration test: create a vault with a known oscillating config, verify the daemon detects and backs off

### Rollout Plan

Ship with the feature. No config needed - cycle detection is always on. The warning log is the only user-visible change.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| False positive: different fixes produce identical file sets | Very Low | Low | Would require same files to have different violations with same paths. Unlikely. |
| User never notices the warning | Medium | Low | The warning logs to the file. Could add a console message on daemon status. |

## Open Questions

None remaining. File paths included in v1 fingerprint after observing real oscillation (97 fixes every sweep).

## References

- Daemon implementation: `src/daemon.rs`
- Feedback loop prevention: AtomicBool `applying` flag (daemon.rs)
- Design doc: `docs/design/2026-03-18-daemon-auto-apply.md`
