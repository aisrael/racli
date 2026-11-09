Feature: racli rename

  The rename command reports the edits that rename one symbol, by name or at a
  position. It never writes them. A name with more than one definition is an
  error.

  Scenario: renaming a symbol by name lists its edits
    When the following command is run:
      """
      racli rename sys::mkfifo --new-name make_fifo
      """
    Then it should exit with status code 0
    And the output should be
      """
      mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8
        fixtures/queue/src/main.rs:81:22: mkfifo => make_fifo
        fixtures/queue/src/sys.rs:3:8: mkfifo => make_fifo
      """

  Scenario: renaming from a position lists the same edits without a header
    When the following command is run:
      """
      racli rename --path fixtures/queue/src/sys.rs --line 3 --character 8 --new-name make_fifo
      """
    Then it should exit with status code 0
    And the output should be
      """
      fixtures/queue/src/main.rs:81:22: mkfifo => make_fifo
      fixtures/queue/src/sys.rs:3:8: mkfifo => make_fifo
      """

  Scenario: the JSON output lists the edits by file
    When the following command is run:
      """
      racli rename sys::mkfifo --new-name make_fifo --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$.files[*].path" with a value ending with "src/sys.rs"
    And the JSON output should match JSONPath "$.files[0].edits[0].new_text" with a value ending with "make_fifo"

  Scenario: a name with two definitions is an error that lists them
    When the following command is run:
      """
      racli rename mkfifo --new-name make_fifo
      """
    Then it should exit with status code 2
    And the error output should contain "cannot rename: 2 definitions match"
    And the error output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:3:8"
    And the error output should contain "mkfifo [FUNCTION] fixtures/queue/src/sys.rs:25:16"

  Scenario: the new name is required
    When the following command is run:
      """
      racli rename sys::mkfifo
      """
    Then it should exit with status code 2
    And the error output should contain "--new-name"

  Scenario: a name that finds nothing is an error
    When the following command is run:
      """
      racli rename nosuchsymbol --new-name other
      """
    Then the error output should contain "no symbol named nosuchsymbol"

  Scenario: renaming a module lists its edits and the rename of its file
    When the following command is run:
      """
      racli rename sys --new-name plumbing --kind module
      """
    Then it should exit with status code 0
    And the output should be
      """
      sys [MODULE] fixtures/queue/src/main.rs:1:5
        fixtures/queue/src/main.rs:1:5: sys => plumbing
        fixtures/queue/src/main.rs:81:17: sys => plumbing
        fixtures/queue/src/main.rs:86:20: sys => plumbing
        fixtures/queue/src/main.rs:86:38: sys => plumbing
        fixtures/queue/src/main.rs:90:5: sys => plumbing
        fixtures/queue/src/sys.rs => fixtures/queue/src/plumbing.rs
      """

  Scenario: the JSON output lists file renames
    When the following command is run:
      """
      racli rename sys --new-name plumbing --kind module --json
      """
    Then it should exit with status code 0
    And the JSON output should match JSONPath "$.renames[0].old_path" with a value ending with "src/sys.rs"
    And the JSON output should match JSONPath "$.renames[0].new_path" with a value ending with "src/plumbing.rs"
