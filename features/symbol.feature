Feature: racli position commands by symbol name

  The position commands take symbol names as well as, or instead of, a file
  position. An argument that's a Rust name is a symbol; anything else is the
  file for --line and --character. A name can be qualified by a module path
  from the crate root (sys::mkfifo) or by the type or trait that contains it
  (Fifo::name). Each definition gets its own result.

  Scenario: text find-references by name includes the declaration and the call site
    When the following command is run:
      """
      racli find-references sys::mkfifo --text
      """
    Then it should exit with status code 0
    And the output should contain "mkfifo [FUNCTION]"
    And the output should contain "main.rs"

  Scenario: a module path picks one of two definitions with the same name
    When the following command is run:
      """
      racli find-references sys::mkfifo
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].result[*].uri" with a value ending with "main.rs"

  Scenario: an ambiguous name gives a result for each definition
    When the following command is run:
      """
      racli find-references mkfifo
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 2 items

  Scenario Outline: a type or trait qualifies a method
    When the following command is run:
      """
      racli find-references <name>
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].symbol.containerName" with a value ending with "<container>"

    Examples:
      | name        | container |
      | Fifo::name  | Fifo      |
      | Named::name | Named     |

  Scenario: several names give a result for each
    When the following command is run:
      """
      racli find-definition Fifo Named
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 2 items
    And the JSON output should match JSONPath "$[1].symbol.name" with a value ending with "Named"

  Scenario: a definition two names find is run once
    When the following command is run:
      """
      racli find-references mkfifo sys::mkfifo
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 2 items

  Scenario: a position given with a name is labelled with its symbol
    When the following command is run:
      """
      racli find-references Fifo fixtures/queue/src/main.rs --line 85 --character 31
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 2 items
    And the JSON output should match JSONPath "$[1].symbol.containerName" with a value ending with "Fifo"

  Scenario: a position whose symbol a name already found is run once
    When the following command is run:
      """
      racli find-references mkfifo fixtures/queue/src/sys.rs --line 2 --character 7
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 2 items

  Scenario: an unknown name lists the closest symbols
    When the following command is run:
      """
      racli find-references mkfif
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfif; closest:"
    And the error output should contain "mkfifo [FUNCTION]"

  Scenario: an unknown name doesn't stop the others
    When the following command is run:
      """
      racli find-definition Fifo mkfif
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfif"
    And the JSON output should be an array of 1 item

  Scenario: --kind keeps only definitions of that kind
    When the following command is run:
      """
      racli find-definition Fifo --kind struct
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].result[*].uri" with a value ending with "sys.rs"

  Scenario: --in that leaves out every definition lists them
    When the following command is run:
      """
      racli find-references mkfifo --in fixtures/queue/src/main.rs
      """
    Then it should exit with status code 0
    And the error output should contain "no symbol named mkfifo passes --kind and --in; found:"

  Scenario: find-implementations by name finds a trait's impls
    When the following command is run:
      """
      racli find-implementations Named --text
      """
    Then it should exit with status code 0
    And the output should contain "Named [INTERFACE]"
    And the output should contain "sys.rs 35:15-35:19"

  Scenario: call-hierarchy by name shows the caller
    When the following command is run:
      """
      racli call-hierarchy sys::mkfifo --direction incoming
      """
    Then it should exit with status code 0
    And the JSON output should be an array of 1 item
    And the JSON output should match JSONPath "$[0].result.incoming[*].uri" with a value ending with "main.rs"

  Scenario Outline: arguments that don't make a position or a name are usage errors
    When the following command is run:
      """
      racli find-references <args>
      """
    Then it should exit with status code 2
    And the error output should contain "<error>"

    Examples:
      | args                                                                        | error                                      |
      | fixtures/queue/src/sys.rs                                                   | needs --line and --character               |
      | mkfifo --line 2 --character 7                                               | --line and --character need a file         |
      | fixtures/queue/src/sys.rs fixtures/queue/src/main.rs --line 2 --character 7 | only one file can be given                 |
      | --kind function fixtures/queue/src/sys.rs --line 2 --character 7            | --kind and --in only apply to symbol names |
      | mkfifo --line 2                                                             | --character                                |
