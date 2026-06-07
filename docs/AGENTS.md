# AGENTS: docs/

Prose documentation for vertex. The crate-level rustdoc is the source of truth for "what does this type do"; files under `docs/` exist for design context, cross-crate architecture, and operator-facing runbooks that do not fit in a single `lib.rs` header.

Root-level rules in `/AGENTS.md` apply here too. The notes below are the doc-specific overlay.

## Dos

- Link into the codebase by crate name (`vertex-swarm-topology`), not file path, unless the path is the point of the reference.
- Keep `docs/README.md` in sync when you add or rename a page. The quick-links table is hand-maintained.
- Prefer diagrams as fenced ASCII or mermaid blocks (the existing `docs/architecture/overview.md` uses mermaid). They survive rendering everywhere.
- Use `multiaddrs` when you mean libp2p multiaddrs.
- When documenting a protocol, link to the constant that names it (for example `vertex_swarm_net_handshake::PROTOCOL`), not a hardcoded string.

## Donts

- No em-dashes. ASCII hyphen or split the sentence.
- Do not import internal planning labels ("Unit N" style) into shipped docs. Refer to consumers and components by name.
- Do not paste reference material from upstream protocol implementations inline. Describe what vertex does and link out if needed.
- Do not write inline architectural asides about the reference implementation inside operator-facing pages. Keep them in design notes only, at the crate root.
- Do not hard-wrap paragraphs in pages that render on the docs site. One logical line per paragraph.

## Building

- `cargo doc --all-features --no-deps` renders the rustdoc.
- The prose pages under `docs/` are plain markdown and have no build step beyond whatever publishes the docs site.
- When adding a page, add a row to `docs/README.md` and a cross-link from at least one neighbouring page so the page is reachable.

## Book of Swarm

The full text lives at `docs/swarm/reference/book-of-swarm.txt`. Treat it as the conceptual reference for any protocol question. When a doc page cites a chapter, link by section number (`chapter 3.3`) rather than line number; the line numbers in the txt extract are not stable across re-conversions.
