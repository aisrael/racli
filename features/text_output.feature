Feature: racli --text output and 1-based positions

  --text prints locations like grep: a path relative to the current directory,
  then a 1-based line and column, then the source line. --line and --character
  are 1-based too, so a position printed by --text can be passed back in.

  Scenario: find-references prints path:line:col and the source line
    When the following command is run:
      """
      racli find-references --path fixtures/queue/src/sys.rs --line 3 --character 8 --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      fixtures/queue/src/main.rs:81:22:         let _ = sys::mkfifo(PATH, 0o666);
      fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: a symbol name's header gives the position of its definition
    When the following command is run:
      """
      racli find-definition sys::mkfifo --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
        fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: a position printed by --text finds the same symbol when passed back in
    When the following command is run:
      """
      racli find-definition --path fixtures/queue/src/sys.rs --line 3 --character 8 --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      fixtures/queue/src/sys.rs:3:8: pub fn mkfifo(path: &str, mode: u16) -> i32 {
      """

  Scenario: search prints name, kind and path:line:col
    When the following command is run:
      """
      racli search mkfifo --kind all-symbols --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8"
    And the output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:25:16"

  Scenario: call-hierarchy prints path:line:col for each item
    When the following command is run:
      """
      racli call-hierarchy --path fixtures/queue/src/sys.rs --line 3 --character 8 --direction incoming --text
      """
    Then it should exit with status code 0
    And the output should be
      """
      mkfifo (FUNCTION) fixtures/queue/src/sys.rs:3:8
      <- callers:
        main (FUNCTION) fixtures/queue/src/main.rs:58:4
      """

  Scenario: document-symbols prints 1-based ranges
    When the following command is run:
      """
      racli document-symbols fixtures/queue/src/sys.rs --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo (FUNCTION) 3:1-9:2"

  Scenario: JSON keeps LSP's absolute URIs and 0-based ranges
    When the following command is run:
      """
      racli find-definition --path fixtures/queue/src/main.rs --line 81 --character 22 --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$[?@.range.start.line == 2 && @.range.start.character == 7]"
    And the JSON output should match JSONPath "$[*].uri" with a value ending with "/fixtures/queue/src/sys.rs"

  Scenario Outline: 0 isn't a position
    When the following command is run:
      """
      racli find-references --path fixtures/queue/src/sys.rs <position>
      """
    Then it should exit with status code 2
    And the error output should contain "positions are 1-based"

    Examples:
      | position                  |
      | --line 0 --character 8    |
      | --line 3 --character 0    |
