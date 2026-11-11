# ADR 008: Struct Fields Looked Up by Type

- **Date:** 2026-11-11
- **Status:** Accepted.

## Context

[ADR 001](001-lookup-by-symbol.md) decision 4 lets a name be qualified by the type or trait that
contains it, such as `Fifo::name`. racli finds these by searching rust-analyzer's
`workspace/symbol` for the member's name and keeping the symbols whose `containerName` is the
type.

That works for methods and associated functions, but not for struct fields. `racli
find-references Config::precision` fails with `no symbol named Config::precision`, although
`Config` has a field `precision`. The only way to look a field up was a position, from `--path`,
`--line` and `--character`.

The cause is in rust-analyzer. Its `workspace/symbol` never returns fields, even with the
`allSymbols` search kind: a search for a field's name finds nothing. `textDocument/documentSymbol`
does list them, as `FIELD` children of the struct. The `--kind all-symbols` documentation said
fields were included, which was wrong.

## Decisions

### 1. `Type::field` is a name, like `Type::method`

```sh
racli find-references Config::precision
racli find-definition Settings::path --text
```

A field is looked up by its type, as a method is, and every command that takes names accepts it:
`find-definition`, `find-references`, `find-implementations`, `call-hierarchy` and `rename`.

### 2. racli reads fields from the file outline

When a qualified name matches nothing in the symbol search, racli:

1. searches for the container's name and keeps the matches that are types (struct, enum, class
   or interface);
2. fetches the outline of the file that defines each one;
3. finds the outline entry for the type, by its name's range, and keeps its children named as
   given.

Each child becomes a symbol in the type, at the range of the field's name. This is the same
position a symbol search gives, so the commands run from it as they would from any definition. The
same lookup finds enum variants, which the outline also lists as children.

The fallback runs after a module path and a method have both failed. A name that already matched
costs nothing extra. A name that matches nothing costs one outline request per type with that
name, in the workspace first and then in its dependencies, as in ADR 001 decision 3.

### 3. A bare field name is not supported

`racli find-references precision` still finds nothing, since there is no type to read an outline
from and rust-analyzer can't search for fields. Use `Type::field`, or a position.

### 4. The documentation says what `all-symbols` returns

`--kind all-symbols` adds functions, methods, constants and statics. It never returns fields. The
CLI help, the MCP tool text, the usage guide and the skill now say so, and point to `Type::field`.

## Alternatives Considered

- **Only improve the error.** Saying that fields need `--path`, `--line` and `--character` costs
  about ten lines. But a position needs a `search` or an outline first, which is the gap ADR 001
  closed for other symbols.
- **Index every outline up front.** Fetching and caching the outline of every file would also
  support bare field names. It costs a request per file at start-up, and a cache to keep fresh,
  for lookups that need a type to be unambiguous anyway.
- **Ask rust-analyzer to return fields.** Nothing in its search extensions does this, so this
  would need a change in rust-analyzer itself.

## Consequences

- Fields work with every command that takes names, with no new options or output formats.
- A lookup that fails for a field now costs extra outline requests. It is still a single search
  for names that resolve normally.
- A field of a type that has the same name as another type in the workspace gives a result for
  each type that has it, as ADR 001 decision 5 does for any ambiguous name.
- Bare field names stay unsupported.
