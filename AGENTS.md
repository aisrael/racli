## Agent Guidelines

1. Don’t assume. Don’t hide confusion. Surface tradeoffs.
2. Minimum code that solves the problem. Nothing speculative.
3. Touch only what you must. Clean up only your own mess.
4. Define success criteria. Loop until verified.

## Project Guidelines

- This is a Rust codebase. You have access to the racli MCP server and the `racli` CLI.
- To navigate the Rust codebase, use the `racli` skill (`.agents/skills/racli/SKILL.md`). It covers every `racli` command, including `document-symbols`, `call-hierarchy` and the `search --kind` / `--scope` options, and how to read their output. The rules below are a summary.
- When searching for symbols DO NOT USE `grep`. Try `racli search` first. Only fall back to `grep` when `racli search` doesn't return anything meaningful.
- When searching for the definition, DO NOT USE `grep`. First try `racli find-definition` with the symbol's name, e.g. `racli find-definition RacliSession`.
- When searching for usages of a symbol, DO NOT USE `grep`. First try `racli find-references` with the symbol's name, e.g. `racli find-references run_cli_search`. The results include the declaration.
- When searching for the implementations of a trait or type, or the overrides of a trait method, DO NOT USE `grep`. First try `racli find-implementations` with the symbol's name, e.g. `racli find-implementations CallHierarchyBackend`.
- A name can be qualified by a module path from the crate root (`search::run_cli_search`) or by the type or trait that contains it (`CallHierarchyBackend::prepare`), and several names can be given at once. Use `--path <FILE> --line <N> --character <N>` instead only when you already have a position, such as one from a compiler error.

## ADRs

- When key decisions are made by the user, offer to create an ADR if the user hasn't explicitly told you to do so.
- Each new ADR should only refer to previous ADRs.
- Forward-looking statements are ok, just not references to future ADRs.
- ADRs live in `docs/adrs/` as `NNN-short-title.md` (three-digit number, e.g. `001-lookup-by-symbol.md`). Follow the layout of `docs/adrs/001-lookup-by-symbol.md`: a title `# ADR NNN: Title`, Date and Status lines, then Context, Decisions, Alternatives Considered and Consequences.
- When adding an ADR, also add it to the list of ADRs in the README's Architecture section.

## Rust Guidelines

- Document all functions, types, and constants limited to 1-2 sentences
