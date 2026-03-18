# Design Document: V2 Schema Alignment

**Author:** Scott Idler
**Date:** 2026-03-18
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

obsidian-cortex cannot validate the vault's v2 frontmatter schema because it lacks enum definitions for domain, type, origin, and status. It also cannot migrate legacy notes because the migrate module only moves files - it has no field rename/drop logic. This design closes both gaps: config-driven enum validation sourced from the vault's own schema files, and config-driven field migration for straggler legacy notes. The v2 migration script already ran on 2026-03-17 and converted ~843 notes, but a handful of stragglers remain and cortex needs the field transform capability for ongoing governance.

## Problem Statement

### Background

The vault migrated to v2 schema on 2026-03-17 (see `system/2026-03-17-vault-v2-migration.md`). The canonical schema lives in the vault itself:

- `system/frontmatter.md` - field definitions, naming rules, deprecated field table
- `system/domain-values.md` - 10 domain values with tag-to-domain mapping
- `system/type-values.md` - 15 type values with typical origin annotations
- `system/origin-values.md` - 3 origin values (authored, assisted, generated)
- `system/status-values.md` - 4 optional status values

obsidian-borg already writes v2-compliant notes. But cortex - the governance tool meant to enforce this schema - cannot actually validate it.

### Problem

1. **No enum validation.** The `type` field accepts any string. `domain`, `origin`, and `status` are not even recognized as known fields - they fall into the `extra` HashMap. Cortex cannot report "domain 'tech-stuff' is not a valid domain."

2. **Incomplete required fields.** Config says `required: [title, date, type, tags]` but v2 requires `domain` and `origin` too. There are also context-dependent exemptions: `inbox/` notes and `type: daily` notes do not require `domain`.

3. **No field migration capability.** The migrate module can move files and set frontmatter values, but cannot rename or drop fields within existing frontmatter. The bulk v2 migration already ran via a Python script (2026-03-17), but a few stragglers remain (e.g., `habits-tracker.md` still has `folder:`). More importantly, cortex needs this capability for ongoing governance - future schema changes should be migrateable through cortex, not one-off scripts.

4. **Frontmatter struct is incomplete.** `vault.rs::Frontmatter` only extracts title, date, type, tags as known fields. domain, origin, status, source, creator all fall into the untyped `extra` HashMap.

5. **Type-fields config is stale.** Config says `video: [source, channel, url]` - mixing v2 names with deprecated names. The vault schema defines specific type-specific fields for daily (health tracking), book (isbn, publisher, pages, cover), meeting (scope, company, confidential, attendees), and media types (source, creator, duration).

### Goals

- Validate domain, type, origin, status against config-defined allowed values
- Add domain and origin to required fields, with exemptions for inbox and daily notes
- Extract domain, origin, status, source, creator as known fields in the Frontmatter struct
- Implement field rename/drop transforms in the migrate module
- Update type-fields config to match the vault's canonical schema
- Report deprecated fields with auto-fix suggestions

### Non-Goals

- Semantic validation of field values (e.g., URL reachability, date plausibility)
- Automatic domain inference from content (obsidian-borg does this via Fabric)
- Migrating notes with no frontmatter at all (existing `frontmatter.missing` rule handles this)
- Changing vault folder structure (already flat, migration complete)
- Validating daily note health tracking fields (weight, walking, etc.) beyond presence

## Proposed Solution

### Overview

Three workstreams that build on each other:

1. **Schema config** - Add a `schema` section to config defining allowed enum values, with context-dependent required field rules
2. **Frontmatter struct expansion** - Promote domain, origin, status, source, creator from `extra` to typed fields. Add enum validation.
3. **Field migration** - Add `field-renames` and `field-drops` capability to the migration config for in-place frontmatter transforms

### Architecture

No new modules needed. Changes touch:

- `config.rs` - new `SchemaConfig` struct, updated `FrontmatterConfig` with exemptions, expanded `MigrationConfig`
- `vault.rs` - expand `Frontmatter` struct with domain, origin, status, source, creator
- `frontmatter.rs` - enum validation logic, context-dependent required field checks
- `migrate.rs` - field rename/drop transforms (no file moves)
- `obsidian-cortex.yml` - schema section, corrected type-fields, field migration config
- `testutil.rs` - fixture notes with v2 and legacy fields

### Data Model

#### Config: schema section

```yaml
schema:
  domains: [ai, tech, football, work, writing, music, spanish, knowledge, resources, system]
  types: [youtube, article, github, social, book, video, research, daily, meeting, note, vocab, moc, link, poem, system]
  origins: [authored, assisted, generated]
  statuses: [unread, reading, reviewed, starred]
  methods: [http, telegram, clipboard, cli, manual]
```

This is a flat list of allowed values. The canonical descriptions live in the vault schema files - cortex only needs the value lists for validation.

#### Config: updated frontmatter section

```yaml
actions:
  frontmatter:
    required: [title, date, type, domain, origin, tags]
    # Context-dependent exemptions
    exempt:
      # type: daily notes do not require domain (they are structural, not topical)
      daily: [domain]
    # Path-based exemptions (separate from type-based)
    path-exempt:
      "inbox/**": [domain]
    type-fields:
      youtube: [source, creator, duration]
      video: [source, creator, duration]
      article: [source, creator]
      github: [source]
      social: [source]
      research: [source]
      book: [creator]
      meeting: [scope, company]
      link: [source]
    auto-title: true
```

#### Config: field migration

```yaml
migrations:
  - name: v2-field-renames
    field-renames:
      url: source
      author: creator
      uploader: creator
      channel: creator
      duration_min: duration
      trace_id: trace
      folder: domain
    field-drops: [day, time, ww, ref]
```

The `moves` field remains for file relocation (already implemented). `field-renames` and `field-drops` are new and independent - a migration can have moves, field transforms, or both.

#### Rust types

```rust
// config.rs - new
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SchemaConfig {
    pub domains: Vec<String>,
    pub types: Vec<String>,
    pub origins: Vec<String>,
    pub statuses: Vec<String>,
    pub methods: Vec<String>,
}

// config.rs - updated FrontmatterConfig
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct FrontmatterConfig {
    pub required: Vec<String>,
    pub exempt: HashMap<String, Vec<String>>,       // type -> exempt fields
    #[serde(rename = "path-exempt")]
    pub path_exempt: HashMap<String, Vec<String>>,   // glob -> exempt fields
    #[serde(rename = "type-fields")]
    pub type_fields: HashMap<String, Vec<String>>,
    #[serde(rename = "auto-title")]
    pub auto_title: bool,
}

// config.rs - updated MigrationConfig
#[derive(Debug, Deserialize, Default)]
pub struct MigrationConfig {
    pub name: String,
    pub moves: Vec<MigrationMove>,
    #[serde(rename = "field-renames", default)]
    pub field_renames: HashMap<String, String>,
    #[serde(rename = "field-drops", default)]
    pub field_drops: Vec<String>,
}
```

```rust
// vault.rs - expanded Frontmatter
pub struct Frontmatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub note_type: Option<String>,  // "type" in YAML
    pub domain: Option<String>,
    pub origin: Option<String>,
    pub status: Option<String>,
    pub tags: Option<Vec<String>>,
    pub source: Option<String>,
    pub creator: Option<String>,
    pub extra: HashMap<String, serde_yaml::Value>,
}
```

### Validation Logic

#### Required fields with exemptions

```rust
fn is_field_required(field: &str, note: &Note, config: &FrontmatterConfig) -> bool {
    if !config.required.contains(&field.to_string()) {
        return false;
    }

    // Check type-based exemptions
    if let Some(ref note_type) = note.frontmatter.note_type {
        if let Some(exempt_fields) = config.exempt.get(note_type) {
            if exempt_fields.contains(&field.to_string()) {
                return false;
            }
        }
    }

    // Check path-based exemptions (inbox/ notes exempt from domain)
    if field == "domain" && note.path.starts_with("inbox/") {
        return false;
    }

    true
}
```

#### Enum validation

```rust
fn validate_enum(
    field_name: &str,
    value: &str,
    allowed: &[String],
    note: &Note,
    report: &mut Report,
) {
    if !allowed.is_empty() && !allowed.iter().any(|v| v == value) {
        report.add(Violation {
            path: note.path.clone(),
            rule: format!("frontmatter.enum.{field_name}"),
            severity: Severity::Error,
            message: format!(
                "{field_name} '{value}' is not valid; allowed: [{}]",
                allowed.join(", ")
            ),
            fix: None, // enum mismatches need human judgment
        });
    }
}
```

Applied to domain, type, origin, status, and method (when present).

#### Deprecated field detection

Integrated into frontmatter lint. If a note has any key that matches a known deprecated field name, report it:

```rust
const DEPRECATED_FIELDS: &[(&str, &str)] = &[
    ("url", "source"),
    ("author", "creator"),
    ("uploader", "creator"),
    ("channel", "creator"),
    ("duration_min", "duration"),
    ("trace_id", "trace"),
    ("folder", "domain"),
];
const DROPPED_FIELDS: &[&str] = &["day", "time", "ww", "ref"];
```

This gives ongoing detection without requiring the user to run `cortex migrate`.

### Field Migration Logic

In `migrate.rs`, field transforms operate on the raw frontmatter block between `---` delimiters:

```rust
pub fn apply_field_transforms(
    vault_root: &Path,
    notes: &[Note],
    migration: &MigrationConfig,
) -> Result<usize> {
    let mut count = 0;

    for note in notes {
        let abs_path = vault_root.join(&note.path);
        let content = fs::read_to_string(&abs_path)?;
        let (fm_block, before, after) = extract_frontmatter_block(&content)?;

        let mut lines: Vec<String> = fm_block.lines().map(String::from).collect();
        let mut changed = false;

        // Track which target fields already exist to avoid conflicts
        let existing_keys: HashSet<String> = lines.iter()
            .filter_map(|l| l.split(':').next().map(|k| k.trim().to_string()))
            .collect();

        // Apply renames
        for (old_key, new_key) in &migration.field_renames {
            for line in &mut lines {
                if line.starts_with(&format!("{old_key}:")) {
                    if existing_keys.contains(new_key) {
                        // Target field exists - skip rename, report conflict
                        tracing::warn!(
                            path = %note.path.display(),
                            old_key, new_key,
                            "skipping rename: target field already exists"
                        );
                    } else {
                        *line = line.replacen(old_key, new_key, 1);
                        changed = true;
                    }
                }
            }
        }

        // Apply drops
        let original_len = lines.len();
        lines.retain(|line| {
            !migration.field_drops.iter().any(|dk| line.starts_with(&format!("{dk}:")))
        });
        if lines.len() != original_len {
            changed = true;
        }

        if changed {
            let new_content = format!("{before}---\n{}\n---{after}", lines.join("\n"));
            fs::write(&abs_path, new_content)?;
            count += 1;
        }
    }

    Ok(count)
}
```

The corresponding `lint_field_transforms()` reports what would change without modifying files.

#### Edge case: `folder` -> `domain` value transform

When renaming `folder` to `domain`, the old value may be a folder path (e.g., `folder: 🤖 Tech/ai-llm`) rather than a clean enum value. The bulk v2 migration script handled this via a mapping table. For straggler notes, cortex should:

1. Rename the key (`folder:` -> `domain:`)
2. If the value is not a valid domain enum, report it as a `frontmatter.enum.domain` violation on the next lint run

This keeps field migration simple (key rename only) and lets enum validation catch value issues separately. No value transform logic in the migration path.

#### Signature threading

Adding `SchemaConfig` to `lint_frontmatter()` changes its signature from `(notes, config)` to `(notes, config, schema)`. This threads through:

- `run_lint()` in `lib.rs` - already has access to `Config`, passes `config.schema`
- Daemon action runner - already has `Config`, same threading
- Tests - construct a `SchemaConfig` in test setup (TestVault already provides a `Config`)

#### Field ordering in `to_yaml()`

The expanded `Frontmatter::to_yaml()` should emit fields in the vault's canonical order: title, date, type, domain, origin, tags, status, source, method, trace, creator, published, duration, then extra fields alphabetically. This matches what obsidian-borg writes and what the vault schema expects.

### Implementation Plan

**Phase A: Frontmatter struct expansion**

1. Add domain, origin, status, source, creator fields to `Frontmatter` in `vault.rs`
2. Update `Frontmatter::from_value()` to extract new known fields from the YAML mapping
3. Update `Frontmatter::to_yaml()` to serialize new fields in correct order
4. Update `Frontmatter::is_empty()` to check new fields
5. Update all tests that construct or assert on `Frontmatter`
6. Update `testutil.rs` fixture notes with v2 fields (domain, origin)

**Phase B: Schema config and enum validation**

1. Add `SchemaConfig` struct to `config.rs` with `schema` top-level section
2. Add `exempt` field to `FrontmatterConfig`
3. Update `obsidian-cortex.yml` with schema values and corrected type-fields
4. Pass `SchemaConfig` to `frontmatter::lint_frontmatter()` (update signature)
5. Implement `is_field_required()` with exemption logic
6. Implement enum validation for domain, type, origin, status, method
7. Add deprecated field detection (check `extra` keys against known deprecated list)
8. Add new violation rules: `frontmatter.enum.*`, `frontmatter.deprecated.*`
9. Update tests

**Phase C: Field migration**

1. Add `field_renames` and `field_drops` to `MigrationConfig` in `config.rs`
2. Add `extract_frontmatter_block()` helper that returns (fm_text, before, after)
3. Implement `lint_field_transforms()` for dry-run reporting
4. Implement `apply_field_transforms()` for in-place renames/drops
5. Wire into `run_migrate()` in `lib.rs` - run field transforms before file moves
6. Add v2-field-renames migration config as a commented example in default config
7. Update tests with legacy-field fixture notes

**Phase D: Config update for real vault**

1. Update `obsidian-cortex.yml` required fields to `[title, date, type, domain, origin, tags]`
2. Add exemptions for daily and inbox
3. Update type-fields to match vault schema (youtube, video, article, github, social, research, book, meeting, link)
4. Add the schema section with all enum values
5. Update protected files list for new vault paths (system/borg-ledger.md, system/borg-dashboard.md)

## Alternatives Considered

### Alternative 1: Hardcode enums in Rust

- **Description:** Define domain/type/origin/status as Rust enums with `#[derive(Deserialize)]`
- **Pros:** Compile-time safety, exhaustive match
- **Cons:** Adding a domain or type requires recompiling. The vault schema evolves independently of the tool.
- **Why not chosen:** Enum values are data, not code. obsidian-borg treats them as config-driven lists. Cortex should match. The vault's system files are the source of truth.

### Alternative 2: Read enum values from vault system files at runtime

- **Description:** Parse `system/domain-values.md`, `system/type-values.md` etc. to extract allowed values
- **Pros:** Single source of truth - the vault defines its own schema, cortex reads it
- **Cons:** Couples cortex to vault file format. Those files are markdown prose with tables, not structured data. Fragile parsing. Cortex may run before the vault is fully set up.
- **Why not chosen:** Config is more robust. The vault files are for human reference in Obsidian. Keeping enum lists in cortex config is a small, acceptable duplication that avoids brittle markdown parsing.

### Alternative 3: Full YAML re-serialization for field migration

- **Description:** Parse frontmatter as serde_yaml::Value, modify the tree, re-serialize
- **Pros:** Correct handling of complex YAML (nested values, multi-line strings)
- **Cons:** Changes formatting, key ordering, quoting style. The original design doc explicitly warns about this (Risk: "Frontmatter round-trip changes formatting").
- **Why not chosen:** Targeted line-level replacement preserves the author's formatting. This matches the approach already used by `scope.rs::insert_frontmatter_fields()` and `tags.rs::replace_tags_in_frontmatter()`.

### Alternative 4: Separate deprecated-fields lint rule

- **Description:** Add `--rule deprecated` as a distinct lint rule instead of integrating into frontmatter lint
- **Pros:** Can be run independently, cleaner separation
- **Cons:** Deprecated fields are a frontmatter concern. Adding a separate rule fragment the validation surface. Users would need to know to run both `--rule frontmatter` and `--rule deprecated`.
- **Why not chosen:** Deprecated field detection is naturally part of frontmatter validation. It reports alongside other frontmatter issues and benefits from the same exemption logic.

## Technical Considerations

### Dependencies

No new dependencies. All changes use existing serde_yaml, regex, string manipulation, and HashMap.

### Performance

Enum validation is O(n * e) where n = notes, e = max enum size (15 for types). Negligible.

Field migration reads and rewrites each affected file once. With ~600 legacy notes, this is a one-time batch operation completing in under a second.

### Testing Strategy

- Unit tests for enum validation with valid, invalid, and missing values
- Unit tests for `is_field_required()` exemption logic (daily, inbox)
- Unit tests for field rename/drop transforms on raw YAML strings
- Unit tests for conflict detection (rename target already exists)
- TestVault fixture updated with:
  - Notes with valid v2 frontmatter (domain, origin, status)
  - Notes with legacy fields (url, author, duration_min, folder)
  - Notes with invalid enum values (domain: "tech-stuff")
  - A `type: daily` note without domain (should pass)
  - An `inbox/` note without domain (should pass)
- Integration test: run migrate on fixture, verify field renames applied
- Integration test: run lint after migrate, verify notes pass enum validation

### Backward Compatibility

- Expanding the `Frontmatter` struct is additive. Fields that were in `extra` simply move to named fields. Code that accessed `extra.get("domain")` will need updating, but this only exists in `scope.rs` (and only for `scope` and `source` keys, not domain).
- Adding domain/origin to `required` means notes without them will fail lint. This is intentional and desired.
- The `exempt` config field defaults to an empty map, so existing configs that don't specify it continue working.
- `MigrationConfig` gains two new optional fields with `#[serde(default)]`, so existing configs with only `moves` continue to parse.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Field rename matches inside YAML values, not just keys (e.g., a note body mentioning "url:") | Low | High | Only operate within the frontmatter block (between `---` delimiters). Match `^key:` at line start. |
| Rename conflict: note has both `author` and `creator` | Medium | Medium | Check for existing target key before renaming. Skip and report as warning if conflict exists. |
| Expanding Frontmatter struct breaks test assertions on extra | Low | Low | New fields are Option<String> defaulting to None. Update fixture notes to include v2 fields. |
| Config schema values drift from vault system files | Medium | Low | Document that schema values should match vault files. Could add a `cortex schema --check` command later. |
| Daily note exemption logic is too narrow (other types may need exemptions) | Low | Low | The `exempt` config is a generic map from type to exempt field list - easy to extend. |
| Field drops remove fields with multi-line values incorrectly | Medium | Medium | For v2 migration, all dropped fields (day, time, ww, ref) are single-line values. Add multi-line awareness later if needed. |

## Open Questions

- [ ] Should `schema` be a top-level config section or nested under `actions.frontmatter`? Top-level keeps it visible and may be shared by future features (e.g., tag-to-domain inference). Recommendation: top-level.
- [ ] Should `cortex lint` auto-detect and report legacy fields even without a migration configured? Recommendation: yes, via hardcoded deprecated field list in frontmatter validation.
- [ ] When both `author` and `creator` exist on a note, should the rename skip (preserving both) or merge (keeping creator, dropping author)? Recommendation: skip and warn.
- [ ] Should `method` enum validation be added now or deferred? It is a source field, not universal. Recommendation: add it - it's minimal extra work.

## References

- [Original obsidian-cortex design doc](2026-03-16-obsidian-cortex.md)
- [Vault V2 Migration design doc](../../system/2026-03-17-vault-v2-migration.md) (in the vault)
- Vault schema files (source of truth for enum values):
  - `~/repos/scottidler/obsidian/system/frontmatter.md`
  - `~/repos/scottidler/obsidian/system/domain-values.md`
  - `~/repos/scottidler/obsidian/system/type-values.md`
  - `~/repos/scottidler/obsidian/system/origin-values.md`
  - `~/repos/scottidler/obsidian/system/status-values.md`
