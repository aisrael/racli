## Agent Guidelines

1. Don’t assume. Don’t hide confusion. Surface tradeoffs.
2. Minimum code that solves the problem. Nothing speculative.
3. Touch only what you must. Clean up only your own mess.
4. Define success criteria. Loop until verified.

## Project Guidelines

- This is a Rust codebase. You have access to the racli MCP server and the `racli` CLI.
- When searching for symbols DO NOT USE `grep`. Try `racli search` first. Only fall back to `grep` when `racli search` doesn't return anything meaningful.
- When searching for the definition, DO NOT USE `grep`. First try `racli find-definition` with the filename, line number, and character offset.
- When searching for usages of a symbol, DO NOT USE `grep`. First try `racli find-references` with the filename, line number, and character offset. The results include the declaration.
- When searching for the implementations of a trait or type, or the overrides of a trait method, DO NOT USE `grep`. First try `racli find-implementations` with the filename, line number, and character offset.

## Rust Guidelines

- Document all functions, types, and constants limited to 1-2 sentences
