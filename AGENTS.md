# compme — Agent Brief

Inline text-completion engine (Rust). macOS ships first; Windows/Linux are
committed deliverables behind the shared PlatformAdapter contract.

## Objective & roadmap
- docs/ROADMAP.md is the single source of truth for pending work and status.
  Read it before starting non-trivial work; update it when you ship.
- Work from main, commit directly to main.
- All deterministic gates green before commit: cargo fmt, clippy, test.

## Conventions
- Minimal diffs, stdlib first, no speculative abstraction.
- Non-trivial logic ships with a test.

## graphify (optional)
`graphify query|explain|path` CLI available (graph at graphify-out/) — use when
it beats native search for cross-file questions. `graphify update .` refreshes
it (AST-only, free).

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).
