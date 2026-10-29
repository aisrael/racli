@mcp
Feature: racli mcp tools mirror the CLI commands

  Each MCP tool is its CLI command (ADR 006). It takes the command's options
  as parameters, with 1-based line and character, and returns what the command
  prints with --text, or its JSON with format "json". What the command would
  print on stderr comes as a second text block. racli mcp runs in
  fixtures/queue, so paths are relative to it.

  Scenario: get_version says in _meta that rust-analyzer is ready
    When the MCP tool "get_version" is called
    Then the tool result should not be an error
    And the tool result's _meta should have "racli/backend" set to "ready"

  Scenario: find_references by name prints the definition and its references
    When the MCP tool "find_references" is called with:
      """
      {"names": ["sys::mkfifo"]}
      """
    Then the tool result should not be an error
    And the tool result's _meta should have "racli/backend" set to "ready"
    And the output should be
      """
      mkfifo [FUNCTION] src/sys.rs:3:8
        src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
        src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: a position is 1-based, so one printed by a tool can be passed back in
    When the MCP tool "find_definition" is called with:
      """
      {"path": "src/sys.rs", "line": 3, "character": 8}
      """
    Then the tool result should not be an error
    And the output should be
      """
      src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: find_definition by position resolves a call to its declaration
    When the MCP tool "find_definition" is called with:
      """
      {"path": "src/main.rs", "line": 81, "character": 22}
      """
    Then the tool result should not be an error
    And the output should contain "src/sys.rs:3:8: pub fn mkfifo"

  Scenario: format json returns the CLI's JSON, with LSP's URIs and 0-based ranges
    When the MCP tool "find_definition" is called with:
      """
      {"path": "src/main.rs", "line": 81, "character": 22, "format": "json"}
      """
    Then the tool result should not be an error
    And the JSON output should match JSONPath "$[?@.range.start.line == 2 && @.range.start.character == 7]"
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "/fixtures/queue/src/sys.rs"

  Scenario Outline: a type or trait qualifies a method
    When the MCP tool "find_references" is called with:
      """
      {"names": ["<name>"], "format": "json"}
      """
    Then the tool result should not be an error
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].symbol.containerName" with a value ending with "<container>"

    Examples:
      | name        | container |
      | Fifo::name  | Fifo      |
      | Named::name | Named     |

  Scenario: an ambiguous name gives a result for each definition
    When the MCP tool "find_references" is called with:
      """
      {"names": ["mkfifo"], "format": "json"}
      """
    Then the tool result should not be an error
    And the JSON output should be an array of 2 items

  Scenario: a name that finds nothing is an error that lists the closest symbols
    When the MCP tool "find_references" is called with:
      """
      {"names": ["mkfif"]}
      """
    Then the tool result should be an error
    And the error output should contain "no symbol named mkfif; closest:"
    And the error output should contain "mkfifo [FUNCTION]"

  Scenario: a name that finds nothing doesn't stop the others
    When the MCP tool "find_definition" is called with:
      """
      {"names": ["Fifo", "mkfif"], "format": "json"}
      """
    Then the tool result should not be an error
    And the error output should contain "no symbol named mkfif"
    And the JSON output should be an array of 1 item

  Scenario: a position in a file that doesn't exist is an error result with the reason
    When the MCP tool "find_references" is called with:
      """
      {"path": "src/nope.rs", "line": 3, "character": 8}
      """
    Then the tool result should be an error
    And the error output should contain "cannot canonicalize src/nope.rs"

  Scenario: a position in a file that doesn't exist doesn't stop the names
    When the MCP tool "find_definition" is called with:
      """
      {"names": ["Fifo"], "path": "src/nope.rs", "line": 3, "character": 8, "format": "json"}
      """
    Then the tool result should not be an error
    And the error output should contain "cannot canonicalize src/nope.rs"
    And the JSON output should be an array of 1 item

  Scenario: kind keeps only definitions of that kind
    When the MCP tool "find_definition" is called with:
      """
      {"names": ["Fifo"], "kind": "struct", "format": "json"}
      """
    Then the tool result should not be an error
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].result[*].uri" with a value ending with "sys.rs"

  Scenario: glob is relative to the workspace root
    When the MCP tool "find_references" is called with:
      """
      {"names": ["unlink"], "glob": "src/*.rs", "format": "json"}
      """
    Then the tool result should not be an error
    And the JSON output should be an array of 2 items

  Scenario: find_definition with source prints the definition's whole source
    When the MCP tool "find_definition" is called with:
      """
      {"names": ["sys::mkfifo"], "source": true}
      """
    Then the tool result should not be an error
    And the output should be
      """
      mkfifo [FUNCTION] src/sys.rs:3:8
        src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
          3  pub fn mkfifo(path: &str, mode: u16) -> i32 {
          4      let c_path = CString::new(path).unwrap();
          5      let p_path = c_path.as_ptr();
          6      unsafe {
          7          return raw::mkfifo(p_path, mode as libc::mode_t);
          8      }
          9  }
      """

  Scenario: find_definition with hover prints the signature and doc comment
    When the MCP tool "find_definition" is called with:
      """
      {"names": ["Named"], "hover": true}
      """
    Then the tool result should not be an error
    And the output should contain "pub trait Named"
    And the output should contain "Names a queue."

  Scenario: find_implementations by name lists the impl blocks
    When the MCP tool "find_implementations" is called with:
      """
      {"names": ["Named"]}
      """
    Then the tool result should not be an error
    And the output should contain "Named [INTERFACE] src/sys.rs"

  Scenario: call_hierarchy by name prints path:line:col for each item
    When the MCP tool "call_hierarchy" is called with:
      """
      {"names": ["sys::mkfifo"], "direction": "incoming"}
      """
    Then the tool result should not be an error
    And the output should be
      """
      mkfifo (FUNCTION) src/sys.rs:3:8
      <- callers:
        main (FUNCTION) src/main.rs:58:4
      """

  Scenario: search with exact keeps only symbols named exactly as the query
    When the MCP tool "search" is called with:
      """
      {"query": "name", "kind": "all_symbols", "exact": true}
      """
    Then the tool result should not be an error
    And the output should be
      """
      flat: 2 symbol(s)
      name [FUNCTION] src/sys.rs:38:8
      name [FUNCTION] src/sys.rs:32:8
      """

  Scenario: document_symbols prints 1-based ranges
    When the MCP tool "document_symbols" is called with:
      """
      {"path": "src/sys.rs"}
      """
    Then the tool result should not be an error
    And the output should contain "mkfifo (FUNCTION) 3:1-9:2"

  Scenario Outline: arguments the command would reject are an error result with the reason
    When the MCP tool "find_references" is called with:
      """
      <arguments>
      """
    Then the tool result should be an error
    And the output should contain "<message>"

    Examples:
      | arguments                                                             | message                                     |
      | {"path": "src/sys.rs", "line": 0, "character": 8}                     | 1-based                                     |
      | {"filePath": "src/sys.rs", "line": 3, "character": 8}                 | path, line and character are given together |
      | {"path": "src/sys.rs", "line": 3}                                     | path, line and character are given together |
      | {"glob": "src/**", "path": "src/sys.rs", "line": 3, "character": 8}   | kind and glob only apply to symbol names    |
      | {"names": ["src/sys.rs"]}                                             | isn't a Rust name                           |
      | {"names": ["mkfifo"], "glob": "["}                                    | glob                                        |
      | {"names": ["mkfifo"], "format": "csv"}                                | csv                                         |
      | {}                                                                    | names                                       |
