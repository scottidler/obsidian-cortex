# Design Document: Tokio Daemon Refactor with Scheduled Intel

**Author:** Scott Idler
**Date:** 2026-03-19
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Refactor the obsidian-cortex daemon from a synchronous polling loop to an async tokio event loop modeled after Syncthing's architecture. Each concern (file watching, debounced actions, periodic sweep, daily intel, weekly intel) gets its own timer/channel in a single `tokio::select!` loop. This enables time-of-day scheduled intel generation without external schedulers.

## Problem Statement

### Background

The current daemon uses a synchronous `recv_timeout(1s)` polling loop that checks elapsed timestamps every second to decide what to do. This works but is clunky - it polls even when nothing is happening, mixes timing concerns into one code path, and has no way to fire actions at specific times of day.

The immediate need is daily/weekly intel generation at configurable times (e.g., daily digest at 23:00, weekly review Sunday 22:00). The current architecture can't express this without checking the clock every second and comparing against target times.

### Problem

1. **No time-of-day scheduling** - the daemon only understands intervals (every N seconds), not wall-clock times
2. **Polling loop is wasteful** - `recv_timeout(1s)` wakes every second to check timestamps even when idle
3. **Timing concerns are tangled** - debounce, sweep interval, and event processing are all interleaved in one code path with manual `Instant::now()` tracking
4. **Adding new scheduled concerns requires more manual timestamp tracking** - each new timer means another `last_X` variable and another `if elapsed >= X` check

### Goals

- Scheduled daily intel generation at a configurable time of day
- Scheduled weekly intel generation at a configurable day and time
- Clean separation of timing concerns (one timer/channel per concern)
- No external schedulers needed (no systemd timers, no cron)
- Preserve existing behavior: file watching, debouncing, periodic sweep, cycle detection

### Non-Goals

- Exposing raw cron expression syntax to users (translated internally via croner)
- Multiple daily/weekly schedules (one of each is sufficient)
- Changing the action execution logic (`run_configured_actions` stays the same)
- Async action execution (actions still run synchronously, only the event loop is async)

## Proposed Solution

### Overview

Replace the synchronous `loop { recv_timeout(1s) }` with an async `loop { tokio::select! { ... } }`. Each timing concern becomes an independent timer or channel in the select. Modeled directly on Syncthing's `folder.Serve()` pattern.

### Architecture

```
tokio::select! loop
  |
  |-- watch_rx.recv()          File watcher events (from notify)
  |-- debounce_timer           Fires after debounce_secs of quiet
  |-- sweep_interval.tick()    Periodic full sweep (poll-interval)
  |-- daily_timer              Next daily intel time
  |-- weekly_timer             Next weekly intel time
  |-- shutdown_rx.recv()       Graceful shutdown (SIGTERM)
```

**Key insight from Syncthing:** Each concern owns its timer. The select loop just dispatches. No manual elapsed-time tracking.

**Concrete select loop sketch:**

```rust
let (watch_tx, mut watch_rx) = tokio::sync::mpsc::unbounded_channel();
let mut sweep_interval = tokio::time::interval(poll_interval);
let mut debounce = tokio::time::sleep(Duration::MAX); // inert until event
tokio::pin!(debounce);
let mut daily = tokio::time::sleep(duration_until_daily(&config.daily_at));
tokio::pin!(daily);
let mut weekly = tokio::time::sleep(duration_until_weekly(&config.weekly_at));
tokio::pin!(weekly);
let mut pending: Vec<PathBuf> = Vec::new();

loop {
    tokio::select! {
        Some(event) = watch_rx.recv() => {
            // Accumulate changed paths, reset debounce timer
            pending.extend(extract_md_paths(&event));
            debounce.as_mut().reset(Instant::now() + debounce_duration);
        }
        _ = &mut debounce, if !pending.is_empty() => {
            // Debounce fired - process pending changes
            run_configured_actions(vault_root, config, &pending);
            pending.clear();
            debounce.as_mut().reset(Instant::now() + Duration::MAX);
        }
        _ = sweep_interval.tick() => {
            // Periodic full sweep with cycle detection
            run_configured_actions(vault_root, config, &[]);
        }
        _ = &mut daily => {
            // Daily intel
            run_intel(vault_root, config, &IntelOpts { daily: true, .. });
            daily.as_mut().reset(Instant::now() + duration_until_daily(&config.daily_at));
        }
        _ = &mut weekly => {
            // Weekly intel
            run_intel(vault_root, config, &IntelOpts { weekly: true, .. });
            weekly.as_mut().reset(Instant::now() + duration_until_weekly(&config.weekly_at));
        }
        _ = tokio::signal::ctrl_c() => break,
    }
}
```

### Timer Design

**Debounce timer:** Created when first file event arrives. Reset on each subsequent event. Fires after `debounce_secs` of quiet. When it fires, process all pending changes and run enabled actions.

**Sweep interval:** `tokio::time::interval(poll_interval)`. Fires every N seconds regardless of other activity. Runs full sweep with cycle detection.

**Daily timer:** Uses human-friendly schedule format (e.g., "M-F 07:00") translated to cron via `schedule_to_cron()` and resolved via `croner` crate's `find_next_occurrence()`. After firing, recompute for next occurrence. Uses `tokio::time::sleep()`.

**Weekly timer:** Same pattern - e.g., "Sun 22:00" is translated to cron "0 22 * * 0" and resolved via croner. After firing, recompute for next week.

### Config Changes

```yaml
daemon:
  debounce-secs: 5
  poll-interval: 300
  daily-at: "M-F 07:00"    # daily intel fires at 7am local, weekdays only
  weekly-at: "Sun 22:00"   # weekly intel fires Sunday 10pm local
  actions:
    lint:
      enable: true
    link:
      enable: true
    duplicates:
      enable: true
    quality:
      enable: true
    auto-tag: {}
    intel:
      enable: true
```

Schedule fields are top-level daemon config. The `intel` action's `enable` flag controls whether intel runs at all. The `daily-at` and `weekly-at` fields control when. If `intel` is enabled but no schedule is set, intel runs on the periodic sweep (existing behavior).

### Data Model

Schedule fields live on `DaemonConfig`, not `DaemonAction`. Scheduling is a daemon-level concern - individual actions just have `enable`. Only intel uses schedules today, but the fields are generic enough to reuse later.

```rust
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    pub actions: HashMap<String, DaemonAction>,
    #[serde(rename = "debounce-secs")]
    pub debounce_secs: u64,
    pub watch: String,
    #[serde(rename = "poll-interval")]
    pub poll_interval: u64,
    #[serde(rename = "daily-at")]
    pub daily_at: Option<String>,     // "M-F 07:00" or "Mon-Fri 07:00" or bare "07:00"
    #[serde(rename = "weekly-at")]
    pub weekly_at: Option<String>,    // "Sun 22:00" or "Sat-Sun 10:00"
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DaemonAction {
    pub enable: bool,  // unchanged
}
```

Helper functions:

```rust
/// Translate human-friendly schedule to cron: "M-F 07:00" -> "0 7 * * 1-5"
pub fn schedule_to_cron(schedule: &str) -> String

/// Compute Duration until next occurrence using croner crate.
pub fn duration_until_next(schedule: &str) -> Duration
```

**Entry point:** `main()` gains `#[tokio::main]` attribute. Non-daemon commands (lint, intel, etc.) run synchronously inside the async runtime - no change to their behavior.

### Implementation Plan

**Phase 1: Async event loop**

Convert `start_watching` from sync to async. Replace `mpsc::channel` + `recv_timeout` with `tokio::sync::mpsc` + `tokio::select!`. Preserve all existing behavior - just change the scheduling mechanism.

- `notify::RecommendedWatcher` feeds events into a `tokio::sync::mpsc::UnboundedSender`
- Debounce uses `tokio::time::sleep` with pin + reset
- Sweep uses `tokio::time::interval`
- Graceful shutdown via `tokio::signal::ctrl_c()`

**Phase 2: Scheduled intel timers**

Add `daily_timer` and `weekly_timer` to the select loop. Parse `daily-at` and `weekly-at` from config. Compute initial sleep duration, fire intel, recompute for next occurrence.

**Phase 3: Simplify suppression**

Drop the `AtomicBool` applying flag. Since the select loop runs in a single tokio task, actions execute inline - the loop naturally doesn't process new events while an action runs. The notify watcher callback runs on a separate OS thread and sends to an unbounded channel (`tokio::sync::mpsc::unbounded_channel`), so events buffer safely during action execution. When the action completes, the loop drains buffered events on the next iteration.

## Alternatives Considered

### Alternative 1: Systemd timers for intel

- **Description:** Keep daemon sync, add separate `.timer` units for daily/weekly intel
- **Pros:** Zero code change to daemon, proven scheduling
- **Cons:** 5 systemd units to manage (service + 2 timers + 2 oneshot services), config split across two systems, `daemon --install` becomes complex
- **Why not chosen:** Splits scheduling config away from cortex's own config file

### Alternative 2: Clock-checking in sync loop

- **Description:** Add `daily_at` check to the existing `recv_timeout(1s)` loop - compare wall clock every second
- **Pros:** Minimal refactor, no async
- **Cons:** Clunky, wastes CPU checking time every second, another manual timestamp variable, doesn't solve the fundamental architecture issue
- **Why not chosen:** Adds to the existing mess rather than cleaning it up

### Alternative 3: Separate scheduler thread

- **Description:** Spawn a dedicated thread that sleeps until next scheduled time, sends wake events to main loop
- **Pros:** No async needed
- **Cons:** Thread management complexity, need cross-thread communication, still have the clunky sync loop for everything else
- **Why not chosen:** Half-measure - if we're adding complexity, make it the right complexity

## Technical Considerations

### Dependencies

- **tokio** - already in Cargo.toml with `full` features
- **notify** - already used, has async-compatible API (watcher callback sends to tokio channel)
- **chrono** - already used, needed for time-of-day parsing
- **croner** - cron expression parser with `find_next_occurrence()` API, handles DST and date math

### Performance

- Async select is more efficient than 1s polling - sleeps until actual event
- Actions still run synchronously (blocking the tokio task) - this is fine since they're I/O bound on disk and fabric calls, and we don't need concurrent action execution
- Channel buffering during action execution prevents event loss

### Testing Strategy

- **Unit tests:** `duration_until_daily` and `duration_until_weekly` computation with various times/days
- **Integration test:** Verify daemon processes file events and runs actions (existing test pattern)
- **Manual test:** Configure `daily-at` to a few minutes from now, verify intel fires

### Rollout Plan

Single commit per phase. Phase 1 is a pure refactor (behavior unchanged). Phase 2 adds the new feature. Phase 3 simplifies suppression.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Async refactor breaks existing daemon behavior | Medium | High | Phase 1 is pure refactor with same test suite, manual smoke test |
| Timer drift over days/weeks (system sleep, NTP jumps) | Low | Low | Recompute next fire time after each trigger, don't accumulate |
| Blocking actions starve the event loop | Low | Low | Actions are fast (disk I/O), fabric has timeout, channel buffers events |
| DST transitions cause double/missed fires | Low | Low | Use UTC internally, convert to local only for display |
| notify crate async compat issues | Low | Medium | Watcher callback is sync but sends to async channel - proven pattern |
| Missed schedule after daemon restart | Medium | Low | On startup, check `.cortex/intel-last-daily` timestamp - if stale, fire immediately |
| System suspend causes delayed fire | Low | None | tokio::time::sleep fires immediately after deadline passes - desired behavior |

## Open Questions

- [x] Should intel run on sweep interval if no schedule is set? **Yes, preserves backward compat**
- [x] Should daily-at/weekly-at be specific to intel, or generic for any action? **Specific to intel for now, on DaemonConfig. Can generalize later if needed.**
- [x] Should we persist last-run timestamps to `.cortex/` for crash recovery? **Yes - write `.cortex/intel-last-daily` and `.cortex/intel-last-weekly` after each run. On startup, if timestamp is stale, fire immediately.**
- [x] UTC or local time for schedule config? **Local time. Users think in local time. DST edge cases are rare and low-impact (worst case: fires an hour early/late twice a year).**

## References

- Syncthing folder.go event loop: `~/repos/syncthing/syncthing/syncthing/lib/model/folder.go:178-256`
- Syncthing timer reschedule with jitter: `~/repos/syncthing/syncthing/syncthing/lib/model/folder.go:324-333`
- Current daemon implementation: `src/daemon.rs:52-166`
- tokio::select! docs: https://docs.rs/tokio/latest/tokio/macro.select.html
- Phase 2 design doc: `docs/design/2026-03-18-phase2-llm-features.md`
