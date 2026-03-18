# Obsidian CLI vs obsidian-cortex: Architecture Analysis

Date: 2026-03-17

## Background

Obsidian shipped a native CLI in v1.12.0 (Feb 2026). Steph Ango (kepano) also published
[kepano/obsidian-skills](https://github.com/kepano/obsidian-skills) - Agent Skill files
that teach Claude Code the CLI syntax. This document captures what the CLI does, how it
works internally, where it overlaps with cortex, and what cortex should (and should not)
adopt from it.

## Official Obsidian CLI

### What it is

The `obsidian` binary IS the Obsidian Electron app. When invoked from a terminal, it
spawns an Electron process, detects the already-running instance via a Unix domain socket
(`SingletonSocket` at `/tmp/scoped_dir*/SingletonSocket`), sends the command, and the
running instance executes it **inside the renderer process** that holds the vault in memory.

This is not a client-server HTTP API. It is Electron's built-in single-instance application
lock mechanism repurposed for CLI command dispatch.

### Command surface

100+ commands organized into:

| Category | Examples |
|----------|---------|
| Files & Folders | `read`, `create`, `append`, `move`, `rename`, `delete`, `files`, `folders` |
| Search | `search`, `search:context` (full-text, uses Obsidian's internal search engine) |
| Daily Notes | `daily:read`, `daily:append`, `daily:prepend` |
| Properties | `property:set`, `property:get`, `property:remove`, `properties` |
| Tags & Links | `tags`, `tag`, `backlinks`, `links`, `orphans`, `deadends`, `unresolved` |
| Tasks | `task`, `tasks` (list, toggle, filter by status) |
| Bases | `bases`, `base:query`, `base:create`, `base:views` |
| Plugins/Themes | `plugin:enable`, `plugin:reload`, `theme:set`, etc. |
| Sync & Publish | `sync:status`, `sync:history`, etc. |
| Developer | `eval`, `dev:errors`, `dev:screenshot`, `dev:dom`, `dev:css`, `dev:console` |

Syntax uses `=` notation: `obsidian create name="My Note" content="Hello"`.
Flags are bare booleans: `silent`, `overwrite`, `total`.

### kepano/obsidian-skills

Repo at [github.com/kepano/obsidian-skills](https://github.com/kepano/obsidian-skills)
(14.4k stars, MIT). Five Agent Skill files (.md docs that teach AI agents correct syntax):

| Skill | Purpose |
|-------|---------|
| obsidian-cli | CLI command reference |
| obsidian-markdown | Obsidian-flavored markdown (wikilinks, callouts, embeds, etc.) |
| obsidian-bases | `.base` file format |
| json-canvas | `.canvas` file format |
| defuddle | Clean markdown extraction from web pages |

These are NOT MCP servers. They are skill definition files with zero runtime dependency.
Cloned locally to `~/repos/kepano/obsidian-skills`.

## How the CLI Uses Obsidian's Index

### MetadataCache internals (confirmed via live `obsidian eval`)

Obsidian maintains an in-memory index via `MetadataCache`. A background Web Worker
(`app.metadataCache.worker`) parses files and fires events (`changed`, `resolve`,
`resolved`) as indexing completes.

Per-file metadata (`CachedMetadata` interface):
- `links` - all wikilinks/markdown links with positions
- `embeds` - all embedded content
- `tags` - all inline tags with positions
- `headings`, `frontmatter`, `frontmatterLinks`
- `blocks`, `sections`, `listItems`, `footnotes`

Vault-wide graph structures:
- `resolvedLinks` - `Record<string, Record<string, number>>` mapping every source to every
  destination with link counts. This IS the link graph.
- `unresolvedLinks` - same structure for broken links
- `getBacklinksForFile(file)` - pre-computed incoming links
- `getTags()` - all tags with counts

### Verified against our vault (835 markdown files, 913 total files)

```
app.metadataCache.resolvedLinks    -> 836 source entries
app.metadataCache.unresolvedLinks  -> 836 entries
app.metadataCache.getCachedFiles() -> 913 files
app.metadataCache.metadataCache    -> 822 entries
app.metadataCache.fileCache        -> 913 entries
```

### Measured operation times (our vault)

| Operation | Time | Mechanism |
|-----------|------|-----------|
| Tags lookup (4,076 tags) | ~12ms | `getTags()` from pre-built cache |
| Backlinks (89 links) | ~3ms | `getBacklinksForFile()` from resolved graph |
| Resolved links traversal | ~0.6ms | iterate `resolvedLinks` map |
| Unresolved links traversal | ~0.6ms | iterate `unresolvedLinks` map |
| Deadends computation | ~0.5ms | check `resolvedLinks` per file |
| Orphans computation | ~214ms | iterate all `resolvedLinks` values |
| Full metadata iteration | ~1ms | 835 files |

The ~2 second wall-clock time visible on CLI commands is **Electron startup overhead**
(spawning a process, connecting to the socket, IPC round-trip), not the operation itself.

### Text search caveat

`obsidian search` runs in-process with access to `Vault.cachedRead()` (avoids redundant
disk reads for unchanged files). However, there is no evidence of a full-text inverted
index - it likely scans cached file contents. Fast because everything is already in memory
after startup, but not indexed in the traditional search-engine sense.

## Overlap with obsidian-cortex

### What the CLI provides that cortex currently does via raw file scanning

| cortex does manually | CLI equivalent |
|---------------------|----------------|
| Wikilink scanning via regex | `backlinks`, `links`, `orphans`, `unresolved` |
| Tag parsing from frontmatter | `tags sort=count counts`, `tag name=X` |
| Broken link detection | `unresolved` |
| Dead-end detection | `deadends` |
| File search/traversal | `search`, `files`, `folders` |
| Frontmatter field reads | `properties`, `property:read` |

### What cortex does that the CLI cannot

- Naming enforcement (lowercase-hyphenated slug validation and rename)
- Frontmatter validation (required fields per note type, auto-derive title)
- Tag normalization (alias resolution, canonical tag list, orphan tag detection)
- Scope classification (work/personal, company, confidential)
- Duplicate detection (exact hash + fuzzy TF-IDF cosine similarity)
- Wikilink inference (suggest links based on title/entity mentions)
- Intelligence generation (daily digest, weekly review, Fabric integration)
- Daemon mode with auto-fix on filesystem events
- Config-driven vault migration (glob-based moves with frontmatter updates)
- Headless operation (no GUI dependency)

### Critical constraint: Obsidian must be running

The CLI requires the Obsidian desktop app to be open. No headless/daemon mode exists.
cortex works anywhere - CI, cron, SSH sessions, systemd services, machines without a
display server. This is a fundamental architectural difference that prevents cortex from
depending on the CLI as its sole backend.

## Existing MCP setup

The `mcp__obsidian__*` tools in our environment (read-note, create-note, edit-note,
search-vault, etc.) are from a **third-party npm package** (`obsidian-mcp` by
StevenStavrakis). It reads files from disk directly - it does NOT use the official CLI
or MetadataCache. This is a separate integration path from the official CLI.

## Performance: Could Rust Match or Beat These Numbers?

### Short answer: yes, easily

The MetadataCache operations are hashmap lookups and graph traversals in JavaScript
running inside an Electron renderer. The measured times (0.5ms-214ms) are not impressive
for what they are doing. A purpose-built Rust index using `FxHashMap` or similar would
match or exceed these on the same data.

### What cortex already has

cortex already parses every note during `walkdir` traversal - frontmatter, wikilinks,
tags, body text. This is 80% of the way to an in-memory index. The parsed data is
currently used for a single lint/link/intel pass and discarded.

### Recommendation: hot data in daemon mode, not a standalone index

**Do:** Keep the parsed vault in memory during daemon mode. The daemon already watches
files and re-parses on change. Every lint/link/intel operation should read from hot data
instead of re-scanning. This is the natural "index" without the ceremony of a persisted
cache format, serialization, corruption recovery, or cache invalidation.

**Do:** For CLI (one-shot) mode, continue using manifest-based change detection
(path + size + mtime). Only re-parse what changed since last run. At 835 files, a full
cold parse is still imperceptible in Rust.

**Don't:** Build a standalone persisted index as a project goal. Two sources of truth
(cortex index vs Obsidian MetadataCache) will drift. The vault is small enough that
full scans are cheap. Solve persistence if/when the vault grows to 10k+ files and cold
starts become noticeable.

**Don't:** Try to replicate Obsidian's full-text search. Obsidian keeps file contents in
memory via `cachedRead`. Replicating that means either mmapping everything or building a
content index (tantivy, etc.), which is significant scope. cortex doesn't need full-text
search - it needs structural queries (tags, links, frontmatter), which are cheap to
compute from parsed note data.

### Where the Obsidian CLI is genuinely better

1. **Search** - file contents cached in memory, no disk I/O on query
2. **Backlinks accuracy** - resolves wikilinks using the same algorithm as the editor,
   including disambiguation logic and alias resolution
3. **Plugin-aware** - can invoke plugin commands, access Bases, work with Sync/Publish
4. **Zero parsing cost** - the index is already built and maintained incrementally as
   part of normal Obsidian operation

### Where cortex is genuinely better

1. **Headless** - works without a GUI, in CI/cron/systemd/SSH
2. **Governance** - rules engine for naming, frontmatter, tags, scope
3. **Custom index fields** - can index things Obsidian doesn't track (tag aliases,
   scope classification, naming violations, duplicate hashes, TF-IDF vectors)
4. **Auto-fix** - dry-run/apply model for automated remediation
5. **Daemon with actions** - not just watching, but enforcing on change
6. **Speed ceiling** - Rust's performance headroom means cortex can do more work per
   file-change event than Obsidian's JS-based incremental updates

## The "70,000x Cheaper" Claim

Could not be traced to any official Obsidian source. Not in the obsidian-skills repo,
official docs, or any blog post by kepano. Likely originates from the YouTube video that
was ingested, referring to **LLM token savings**: `obsidian tags total` returns a single
number (a few bytes) vs dumping 835 raw markdown files to extract tags (millions of
tokens). This is about AI agent efficiency, not computational performance.

## Related Notes in Vault

The vault contains ~25 notes on Obsidian + Claude/AI integration, including:

- `vault-governance-brainstorm.md` - genesis doc for what became obsidian-cortex
- `how-i-use-obsidian-claude-code-to-run-my-life.md` - Greg Isenberg / Internet Vin
- `i-built-my-second-brain-with-claude-code-obsidian-skills-here-s-how.md` - Cole Medin
- `claude-code-obsidian-ultimate-ai-life-os.md` - Eric Michaud's 27-command system
- `obsidian-ai-how-to-do-it-the-right-way.md` - Nick Milo's IDI framework
- `how-to-get-ai-in-obsidian.md` - Nick Milo setup tutorial
- `obsidian-cli-claude-code-your-ai-agent-now-controls-your-notes-70-000x-cheaper.md` -
  the article that prompted this analysis

## Conclusion

The official Obsidian CLI and obsidian-cortex are **complementary, not competing**. The CLI
is a read/write interface backed by Obsidian's live in-memory index. cortex is a governance
engine that enforces rules and adds intelligence. The CLI answers "how do I interact with
my vault?" - cortex answers "how do I keep my vault clean and consistent?"

cortex should:
1. Keep parsed vault data hot in daemon mode (cheap, no new dependencies)
2. Continue owning governance logic that the CLI cannot provide
3. Optionally shell out to the CLI for operations where Obsidian's index is more accurate
   (backlink resolution, search) - but only when Obsidian is confirmed running
4. Never depend on the CLI being available - headless operation is a core differentiator
