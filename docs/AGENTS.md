# AGENTS: docs/

Prose documentation for vertex. Crate-level rustdoc is the source of truth for what a type does; files under `docs/` carry design context, cross-crate architecture, and operator runbooks that do not fit a single `lib.rs` header.

Global rules (em-dashes, multiaddrs-not-underlay, no inline reference-impl asides, no internal plan labels): see root `/AGENTS.md`. The notes below are the doc-specific overlay.

## Rules

- Link into the codebase by crate name (`vertex-swarm-topology`), not file path, unless the path is the point of the reference.
- When documenting a protocol, link to the constant that names it (`vertex_swarm_net_handshake::PROTOCOL`), not a hardcoded string.
- Keep `docs/README.md` in sync when you add or rename a page; its quick-links table is hand-maintained. Every new page gets a row there and a cross-link from at least one neighbouring page so it is reachable.
- Diagrams as fenced ASCII or mermaid (`docs/architecture/overview.md` uses mermaid). They survive rendering everywhere.
- Do not hard-wrap paragraphs in pages that render on the docs site. One logical line per paragraph.

## Building

- `cargo doc --all-features --no-deps` renders the rustdoc.
- Prose pages under `docs/` are plain markdown with no build step beyond whatever publishes the docs site.

## Book of Swarm

The full text lives at `docs/swarm/reference/book-of-swarm.txt`: the conceptual reference for any protocol question. Cite it by section number (`chapter 3.3`), not line number; the line numbers are not stable across re-conversions.
