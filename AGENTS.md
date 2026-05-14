## Agent Guidelines

1. Don’t assume. Don’t hide confusion. Surface tradeoffs.
2. Minimum code that solves the problem. Nothing speculative.
3. Touch only what you must. Clean up only your own mess.
4. Define success criteria. Loop until verified.

## Project Guidelines

This is a Rust codebase. 

When searching for strings, DO NOT USE `grep`, use `racli search`. Only fallback to `grep` when `racli search` doesn't return anything meaningful.

When searching for the definition of a symbol, DO NOT USE `grep`, use `racli find-definition` with the filename, line number, and character offset.

## Rust Guidelines

- Document all functions, types, and constants limited to 1-2 sentences
